"""Git integration bound to task boundaries — invisible to the agent.

Agents never pull and never push directly. Git is touched at exactly three
control-plane boundaries:

* **claim** (`prepare_for_claim`): fast-forward the local tree to the current
  baseline so new work starts from the latest shared state, then the claim
  records that point-in-time commit.
* **phase completion** (`integrate_and_publish`): integrate the completing
  agent's work with the baseline, preserving divergent histories in a
  baseline-first merge and fast-forwarding when the baseline already descends
  from the local line. A real content
  conflict is the one and only thing surfaced to the agent — framed as an
  overlap with the baseline, never as a sync with an upstream.
* **agent launch** (`prepare_for_agent_launch`): fetch and fast-forward a clean,
  uncommitted lane immediately before its supervisor or native harness starts,
  so long-lived processes never import a checkout that was already stale when
  they launched.

The default baseline is the current branch's user-managed merge target on the
conventional ``origin`` remote, or ``origin/HEAD`` when no merge is configured.
When no remote exists (local-only trees, or test harnesses) every operation
degrades to a safe no-op that still records the local HEAD, so the captured
review record holds without a remote.
"""

from __future__ import annotations

import os
import shlex
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from spice.errors import SpiceError
from spice.process.git import DEFAULT_GIT_TIMEOUT_SECONDS, run_git_command
from spice.paths import atomic_write_text
from spice.tasks import config, identity, wordingreview

GIT_NETWORK_TIMEOUT_SECONDS = 30
TASK_GIT_SSH_COMMAND = "ssh -o BatchMode=yes -o ConnectTimeout=5"
_NETWORK_COMMANDS = {"fetch", "push"}
# Publish-race rounds before surfacing recovery guidance: enough for a full
# N-agent completion storm to drain ahead of this push, small enough that a
# genuinely wedged remote fails fast.
PUBLISH_RACE_RETRY_LIMIT = 5
MERGE_STATE_FILES = ("ORIG_HEAD", "MERGE_MODE", "MERGE_MSG", "MERGE_HEAD")


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
    timeout = (
        GIT_NETWORK_TIMEOUT_SECONDS if args and args[0] in _NETWORK_COMMANDS else None
    )
    return run_git_command(
        command,
        default_timeout_seconds=timeout or DEFAULT_GIT_TIMEOUT_SECONDS,
        **kwargs,
    )


def _run_with_input(
    repo_root: Path, *args: str, input_text: str
) -> subprocess.CompletedProcess[str]:
    env = _control_plane_git_env()
    command = ["git", "-C", str(repo_root), *args]
    return run_git_command(
        command,
        default_timeout_seconds=DEFAULT_GIT_TIMEOUT_SECONDS,
        capture_output=True,
        check=False,
        env=env,
        input=input_text,
        text=True,
    )


def _control_plane_git_env() -> dict[str, str]:
    env = dict(os.environ)  # env-policy: allow
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["GIT_SSH_COMMAND"] = TASK_GIT_SSH_COMMAND
    return env


def _read(repo_root: Path, *args: str) -> str:
    completed = _run(repo_root, *args)
    return completed.stdout.strip() if completed.returncode == 0 else ""


def _resolve_target(repo_root: Path) -> tuple[str, str] | None:
    """Return ``(remote, baseline_ref)`` for this worktree's task baseline,
    or ``None`` when the configured remote is absent (local-only).

    The current branch's configured merge is authoritative. Missing merge
    config falls back to ``origin/HEAD`` in remote-backed worktrees.
    """
    upstream = branch_upstream_target(repo_root)
    if upstream is not None:
        return upstream
    if not _read(repo_root, "remote"):
        return None
    raise SpiceError(
        "cannot resolve task baseline: origin remote is unavailable; add an "
        "origin remote, configure branch tracking, or use a local-only tree"
    )


def branch_upstream_target(repo_root: Path) -> tuple[str, str] | None:
    # The lane's user-managed merge (branch.<lane>.merge) is the single source of
    # truth — and it stays readable under the agent shadow: the shadow's
    # self-merge lives in *system* scope, so `git config --get` returns the
    # native value (worktree or common config). The remote is `origin`
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
    return _commits_ahead_of_target(root, resolved)


def _commits_ahead_of_target(repo_root: Path, resolved: tuple[str, str] | None) -> int:
    if resolved is None:
        return 0
    _, baseline = resolved
    ahead = _read(repo_root, "rev-list", "--count", f"{baseline}..HEAD")
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
    blocked = _purge_stale_bytecode(root, before, after)
    notes = ["updated working tree to the current baseline"] if after != before else []
    if blocked:
        notes.append(_bytecode_cleanup_note(blocked))
    return SyncResult(notes=notes)


def prepare_for_agent_launch(repo_root: Path | None = None) -> SyncResult:
    """Strictly synchronize a lane immediately before its agent process starts.

    Launch is the last boundary at which the globally installed control plane
    can update the checkout before ``python -m spice`` and the native harness
    import from it. Fetching is read-only with respect to user work; any dirty,
    ahead, divergent, or unverifiable state refuses the launch instead of
    starting a process from a checkout that is known to be stale.
    """
    root = repo_root or config.repo_root()
    resolved = _resolve_target(root)
    if resolved is None:
        return SyncResult(notes=["current:local-only"])
    remote, baseline = resolved
    fetched = _run(root, "fetch", remote)
    if fetched.returncode != 0:
        raise SpiceError(
            "cannot launch agent: the current baseline could not be fetched; "
            "fix the remote or credentials and retry\n"
            + _fail(f"fetch {remote} for agent launch", fetched)
        )
    if not _read(root, "rev-parse", baseline):
        raise SpiceError(
            f"cannot launch agent: baseline {baseline} was not found after "
            f"fetching {remote}; repair branch tracking and retry"
        )
    if _worktree_dirty(root):
        raise SpiceError(
            "cannot launch agent: the working tree is dirty; commit or clear "
            "the user-owned changes before retrying"
        )
    counts = _read(root, "rev-list", "--left-right", "--count", f"{baseline}...HEAD")
    try:
        behind_text, ahead_text = counts.split()
        behind, ahead = int(behind_text), int(ahead_text)
    except (TypeError, ValueError):
        behind = ahead = 0
    if ahead:
        if behind:
            raise SpiceError(
                f"cannot launch agent: the branch has diverged from {baseline}; "
                "resolve the branch through the task Git control plane and retry"
            )
        raise SpiceError(
            f"cannot launch agent: the branch has {ahead} local commit(s) not "
            "recorded by a completed task; capture or complete that work and retry"
        )
    before = _read(root, "rev-parse", "HEAD")
    completed = _run(root, "merge", "--ff-only", baseline)
    if completed.returncode != 0:
        raise SpiceError(
            f"cannot launch agent: the working tree could not fast-forward to "
            f"{baseline}; repair the branch and retry\n"
            + _fail(f"fast-forward agent launch to {baseline}", completed)
        )
    after = _read(root, "rev-parse", "HEAD")
    blocked = _purge_stale_bytecode(root, before, after)
    notes = [
        "updated working tree to the current baseline" if after != before else "current"
    ]
    if blocked:
        notes.append(_bytecode_cleanup_note(blocked))
    return SyncResult(notes=notes)


def fast_forward_if_safe(repo_root: Path | None = None) -> SyncResult:
    """Bring the tree up to the current baseline when, and only when, it is
    safe.

    Lenient sibling of :func:`prepare_for_claim` for activation: it applies
    the same rules (clean tree, zero commits ahead, fast-forward-only) but
    never raises, so activation always succeeds. Every outcome is reported as
    a note rather than a silent no-op: ``current`` when already up to date,
    or ``skipped:<dirty|ahead|diverged|no-remote>`` for each safe no-op, so a
    non-advance is observable in the activation packet instead of invisible.
    """
    root = repo_root or config.repo_root()
    try:
        resolved = _resolve_target(root)
    except SpiceError:
        return SyncResult(notes=["skipped:no-remote"])
    if resolved is None:
        return SyncResult(notes=["skipped:no-remote"])
    remote, baseline = resolved
    if _worktree_dirty(root):
        return SyncResult(notes=["skipped:dirty"])
    ahead = _read(root, "rev-list", "--count", f"{baseline}..HEAD")
    if ahead and ahead != "0":
        return SyncResult(notes=["skipped:ahead"])
    before = _read(root, "rev-parse", "HEAD")
    _run(root, "fetch", remote)
    if not _read(root, "rev-parse", baseline):
        return SyncResult(notes=["skipped:no-remote"])
    if _run(root, "merge", "--ff-only", baseline).returncode != 0:
        return SyncResult(notes=["skipped:diverged"])
    after = _read(root, "rev-parse", "HEAD")
    blocked = _purge_stale_bytecode(root, before, after)
    notes = [
        "updated working tree to the current baseline" if after != before else "current"
    ]
    if blocked:
        notes.append(_bytecode_cleanup_note(blocked))
    return SyncResult(notes=notes)


def integrate_and_publish(
    label: str,
    repo_root: Path | None = None,
    *,
    meta: dict[str, str] | None = None,
) -> SyncResult:
    """Integrate the completing agent's work with the baseline and publish it.

    Divergent histories land a merge commit with the baseline as first parent
    and the agent's last commit as second parent (``--no-ff`` semantics).
    Tree-equal descendant baselines fast-forward without minting an empty
    merge. The integrated and agent commits are captured for review and pushed.
    A merge commit message is composed from
    harvested task and git facts. A real content conflict raises
    :class:`MergeConflict` with the tree left mid-merge for the agent to
    resolve and commit. A resolution that still contains conflict markers is
    refused before anything publishes, as is a landing whose first-parent
    diff changes baseline paths outside the task's own commits. With no
    configured remote this records the local HEAD and performs no network or
    history mutation.
    """
    root = repo_root or config.repo_root()
    wordingreview.require_integrate_allowed(label, meta)
    agent_head = _read(root, "rev-parse", "HEAD")
    resolved = _resolve_target(root)
    local_commits = _commits_ahead_of_target(root, resolved)
    if resolved is None:
        return SyncResult(
            uda_args=_capture(agent_head, agent_head, "", "", local_commits)
        )
    remote, baseline = resolved

    upstream_head = _fetch_upstream_head(root, remote, baseline)
    if agent_head == upstream_head:
        # Nothing to integrate; the baseline already holds this state.
        return SyncResult(
            uda_args=_capture(
                agent_head,
                agent_head,
                baseline,
                upstream_head,
                local_commits,
            )
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
    tree_already_integrated = _tree_of(root, merge_head) == _tree_of(
        root, upstream_head
    )
    tree_same_note = ""
    if tree_already_integrated:
        tree_same_note = (
            "task tree already integrated on baseline; advanced without rewriting refs"
            if merge_head == upstream_head
            else "task tree already integrated on baseline; preserved divergent "
            "commits in a tree-same merge"
        )
    merge_head, upstream_head = _publish_integrated_task(
        root,
        remote=remote,
        baseline=baseline,
        label=label,
        merge_head=merge_head,
        upstream_head=upstream_head,
        agent_head=agent_head,
        message=message,
    )
    return SyncResult(
        notes=[tree_same_note] if tree_same_note else [],
        uda_args=_capture(
            agent_head,
            merge_head,
            baseline,
            upstream_head,
            local_commits,
        ),
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
    label: str,
    agent_head: str,
    upstream_head: str,
    message: str,
) -> str:
    # Compute first, without touching refs, the index, or the working tree.
    # Porcelain `git merge` writes ORIG_HEAD through the reference-transaction
    # hook and can materialize a conflict before MERGE_HEAD exists. A failed
    # hook therefore used to strand loose markers with neither the complete
    # merged index nor a baseline parent. merge-tree closes that failure
    # window; conflicts are materialized below as a complete merge state.
    if _read(repo_root, "rev-parse", "--verify", "MERGE_HEAD"):
        raise MergeConflict(_merge_conflict_recovery(label, repo_root, upstream_head))
    merge = _run(
        repo_root,
        "merge-tree",
        "--write-tree",
        "-z",
        "--no-messages",
        agent_head,
        upstream_head,
    )
    merged_tree, conflict_records = _parse_merge_tree_output(merge.stdout)
    if merge.returncode == 1:
        _materialize_merge_conflict(
            repo_root,
            merged_tree=merged_tree,
            conflict_records=conflict_records,
            agent_head=agent_head,
            upstream_head=upstream_head,
            message=message,
        )
        raise MergeConflict(_merge_conflict_recovery(label, repo_root, upstream_head))
    if merge.returncode != 0 or not merged_tree:
        raise SpiceError(_fail("compute task merge tree", merge))
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
    agent_head: str,
    message: str,
) -> tuple[str, str]:
    flagged = _conflict_marker_paths(repo_root, upstream_head, merge_head)
    if flagged:
        raise SpiceError(_conflict_marker_refusal(label, flagged))
    _refuse_out_of_scope_landing(
        repo_root,
        label=label,
        upstream_head=upstream_head,
        merge_head=merge_head,
        agent_head=agent_head,
    )
    branch = baseline.split("/", 1)[1]
    return _publish_task_merge(
        repo_root,
        remote=remote,
        baseline=baseline,
        branch=branch,
        label=label,
        merge_head=merge_head,
        upstream_head=upstream_head,
        agent_head=agent_head,
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
    agent_head: str,
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
        agent_head=agent_head,
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
    agent_head: str,
    message: str,
    first_push: subprocess.CompletedProcess[str],
) -> tuple[str, str]:
    # Under an N-agent completion storm every push can lose to a peer that
    # landed just before it, so a single retry punts real convergence to the
    # agent. Re-fetch/re-merge/re-push up to the bound; each round folds the
    # freshly advanced baseline into the same generated merge shape.
    last_push = first_push
    for _ in range(PUBLISH_RACE_RETRY_LIMIT):
        fetch = _run(repo_root, "fetch", remote)
        if fetch.returncode != 0:
            raise SpiceError(_publish_race_recovery(label, remote, baseline, last_push))
        fresh_upstream_head = _read(repo_root, "rev-parse", baseline)
        if not fresh_upstream_head or fresh_upstream_head == previous_upstream_head:
            raise SpiceError(_publish_race_recovery(label, remote, baseline, last_push))
        if fresh_upstream_head == merge_head:
            return merge_head, fresh_upstream_head

        merge = _run(
            repo_root,
            "merge-tree",
            "--write-tree",
            "-z",
            "--no-messages",
            merge_head,
            fresh_upstream_head,
        )
        merged_tree, conflict_records = _parse_merge_tree_output(merge.stdout)
        if merge.returncode == 1:
            _materialize_merge_conflict(
                repo_root,
                merged_tree=merged_tree,
                conflict_records=conflict_records,
                agent_head=merge_head,
                upstream_head=fresh_upstream_head,
                message=message,
            )
            raise MergeConflict(
                _merge_conflict_recovery(label, repo_root, fresh_upstream_head)
            )
        if merge.returncode != 0 or not merged_tree:
            raise SpiceError(_fail("compute publish-race merge tree", merge))
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
        _refuse_out_of_scope_landing(
            repo_root,
            label=label,
            upstream_head=fresh_upstream_head,
            merge_head=retry_head,
            agent_head=agent_head,
        )
        retry_push = _run(repo_root, "push", remote, f"{retry_head}:{branch}")
        if retry_push.returncode == 0:
            return retry_head, fresh_upstream_head
        if not _is_non_fast_forward_push(retry_push):
            raise SpiceError(_fail(f"publish task work to {baseline}", retry_push))
        previous_upstream_head = fresh_upstream_head
        merge_head = retry_head
        last_push = retry_push
    raise SpiceError(_publish_race_recovery(label, remote, baseline, last_push))


def _is_non_fast_forward_push(completed: subprocess.CompletedProcess[str]) -> bool:
    output = (completed.stdout + "\n" + completed.stderr).lower()
    return (
        "non-fast-forward" in output
        or "fetch first" in output
        or "stale info" in output
    )


def _parse_merge_tree_output(output: str) -> tuple[str, list[str]]:
    fields = output.split("\0")
    tree = fields[0].strip() if fields else ""
    return tree, [field for field in fields[1:] if field]


def _materialize_merge_conflict(
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
        materialize = _run(repo_root, "read-tree", "--reset", "-u", merged_tree)
        if materialize.returncode != 0:
            raise SpiceError(_fail("materialize conflicted merge tree", materialize))
        _purge_stale_bytecode(repo_root, agent_head, merged_tree)
        for path in sorted({path for _, path in parsed}):
            removed = _run(repo_root, "update-index", "--force-remove", "--", path)
            if removed.returncode != 0:
                raise SpiceError(_fail(f"prepare conflict index for {path}", removed))
        index_info = "".join(f"{metadata}\t{path}\0" for metadata, path in parsed)
        staged = _run_with_input(
            repo_root, "update-index", "-z", "--index-info", input_text=index_info
        )
        if staged.returncode != 0:
            raise SpiceError(_fail("install conflict index stages", staged))

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
        restored = _run(repo_root, "read-tree", "--reset", "-u", agent_head)
        if restored.returncode != 0:
            raise SpiceError(_fail("restore pre-merge tree", restored))
        _purge_stale_bytecode(repo_root, merged_tree, agent_head)
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
    value = _read(repo_root, "rev-parse", "--git-path", name)
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


def _tree_of(repo_root: Path, ref: str) -> str:
    return _read(repo_root, "rev-parse", f"{ref}^{{tree}}")


def _purge_stale_bytecode(repo_root: Path, before: str, after: str) -> list[str]:
    """Delete bytecode orphaned by a tree move from ``before`` to ``after``.

    Tree moves (``read-tree --reset -u``, ``merge --ff-only``) rewrite tracked
    files only, so untracked ``__pycache__`` entries survive every move. A
    deleted module's bytecode keeps its package directory alive as an
    importable namespace package, and a modified module can be shadowed by
    bytecode whose (mtime, size) validation key still matches. The diff
    between the move's endpoints is the whole truth: every ``.py`` path it
    lists drops its compiled artifacts, and directories that would survive
    only because of that bytecode are pruned.

    Cleanup is strictly best-effort and never raises: diff discovery,
    repository descriptor open, per-source traversal, and descriptor teardown
    failures are all contained here so the surrounding Git transaction stays
    coherent even when cleanup is impossible. Sources whose compiled
    artifacts may survive are returned so callers with a reporting channel
    can surface manual cleanup guidance; when discovery itself fails the
    report names the unknown scope instead of a source list. Skipping
    platforms without descriptor-relative cleanup stays silent because that
    is a permanent capability gap, not a failed cleanup of these sources.
    """
    if not before or not after or before == after:
        return []
    try:
        listing = _read(
            repo_root, "diff", "--name-only", "--no-renames", "-z", before, after
        )
    except (OSError, ValueError, subprocess.SubprocessError, SpiceError):
        return [BYTECODE_SCOPE_UNKNOWN]
    if not _supports_safe_bytecode_purge():
        return []
    candidates: list[tuple[str, Path]] = []
    for name in listing.split("\0"):
        if not name.endswith(".py"):
            continue
        source = Path(name)
        if source.is_absolute() or any(
            part in {"", ".", ".."} for part in source.parts
        ):
            continue
        candidates.append((name, source))
    if not candidates:
        return []
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    try:
        root_fd = _open_worktree_root(repo_root, directory_flags)
    except OSError:
        return [name for name, _ in candidates]
    blocked: list[str] = []
    try:
        for name, source in candidates:
            if _purge_source_bytecode(root_fd, source, directory_flags):
                blocked.append(name)
    finally:
        _close_quietly(root_fd)
    return blocked


def _open_worktree_root(repo_root: Path, directory_flags: int) -> int:
    return os.open(repo_root.resolve(), directory_flags)


def _close_quietly(fd: int) -> None:
    """Best-effort descriptor close: teardown cannot break the purge contract."""
    try:
        os.close(fd)
    except OSError:
        pass


BYTECODE_SCOPE_UNKNOWN = "unidentified modules (cleanup diff unavailable)"


def _bytecode_cleanup_note(blocked: list[str]) -> str:
    listed = ", ".join(sorted(blocked))
    return (
        f"stale bytecode kept for {listed}: automatic cleanup was interrupted; "
        "remove the matching __pycache__ entries manually"
    )


def _supports_safe_bytecode_purge() -> bool:
    """Whether this platform offers descriptor-relative, no-follow cleanup."""
    return bool(
        hasattr(os, "O_DIRECTORY")
        and hasattr(os, "O_NOFOLLOW")
        and os.open in os.supports_dir_fd
        and os.unlink in os.supports_dir_fd
        and os.rmdir in os.supports_dir_fd
        and os.scandir in os.supports_fd
    )


def _purge_source_bytecode(root_fd: int, source: Path, directory_flags: int) -> bool:
    """Purge one source's cache through no-follow directory descriptors.

    Every lookup below the already-open worktree root is relative to a trusted
    directory descriptor. A changed source parent or ``__pycache__`` replaced
    by a symlink therefore stops cleanup instead of redirecting an unlink.
    ``unlinkat`` removes a matching entry itself and never follows a final
    symlink.

    Never raises. Returns True when compiled artifacts may survive because
    the operating system refused a step (permissions, symlink refusal);
    returns False when cleanup completed or there was nothing to clean.
    """
    parent_parts = source.parent.parts
    parent_fds: list[int] = []
    current_fd = root_fd
    try:
        for part in parent_parts:
            try:
                current_fd = os.open(
                    part,
                    directory_flags,
                    dir_fd=current_fd,
                )
            except (FileNotFoundError, NotADirectoryError):
                return False
            except OSError:
                return True
            parent_fds.append(current_fd)
        try:
            cache_fd = os.open(
                "__pycache__",
                directory_flags,
                dir_fd=current_fd,
            )
        except (FileNotFoundError, NotADirectoryError):
            return False
        except OSError:
            return True
        blocked = False
        try:
            prefix = f"{source.stem}."
            with os.scandir(cache_fd) as entries:
                compiled_names = [
                    entry.name for entry in entries if entry.name.startswith(prefix)
                ]
            for compiled_name in compiled_names:
                try:
                    os.unlink(compiled_name, dir_fd=cache_fd)
                except FileNotFoundError:
                    continue
                except OSError:
                    blocked = True
        except OSError:
            blocked = True
        finally:
            _close_quietly(cache_fd)

        try:
            os.rmdir("__pycache__", dir_fd=current_fd)
        except OSError:
            return blocked
        for index in range(len(parent_parts) - 1, -1, -1):
            parent_fd = root_fd if index == 0 else parent_fds[index - 1]
            try:
                os.rmdir(parent_parts[index], dir_fd=parent_fd)
            except OSError:
                break
        return blocked
    finally:
        for parent_fd in reversed(parent_fds):
            _close_quietly(parent_fd)


def _collapse_to_first_parent(repo_root: Path, first_parent: str, *, label: str) -> str:
    """Fast-forward the branch to a tree-equivalent descendant baseline."""
    expected_head = _read(repo_root, "rev-parse", "HEAD")
    _materialize_and_update_head(
        repo_root,
        new_head=first_parent,
        expected_head=expected_head,
        label=label,
        action="fast-forward tree-same phase onto baseline",
    )
    return first_parent


def _synthesize_and_fast_forward(
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
    if _tree_of(repo_root, treeish) == _tree_of(
        repo_root, first_parent
    ) and _is_ancestor(repo_root, "HEAD", first_parent):
        return _collapse_to_first_parent(repo_root, first_parent, label=label)
    merge_head = _synthesize_merge(
        repo_root, treeish, first_parent, second_parent, message
    )
    expected_head = _read(repo_root, "rev-parse", "HEAD")
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
    current_ref = _read(repo_root, "symbolic-ref", "--quiet", "HEAD")
    if not current_ref:
        raise SpiceError(f"cannot {action}: HEAD is detached")
    materialize = _run(repo_root, "read-tree", "--reset", "-u", new_head)
    if materialize.returncode != 0:
        restored = _run(repo_root, "read-tree", "--reset", "-u", expected_head)
        if restored.returncode != 0:
            raise SpiceError(_fail(f"restore tree after failed {action}", restored))
        _purge_stale_bytecode(repo_root, expected_head, new_head)
        raise SpiceError(_fail(action, materialize))
    _purge_stale_bytecode(repo_root, expected_head, new_head)

    update = _run(repo_root, "update-ref", current_ref, new_head, expected_head)
    current_head = _read(repo_root, "rev-parse", "HEAD")
    if update.returncode == 0 or current_head == new_head:
        return

    restore_head = current_head or expected_head
    restored = _run(repo_root, "read-tree", "--reset", "-u", restore_head)
    if restored.returncode != 0:
        raise SpiceError(_fail(f"restore tree after failed {action}", restored))
    _purge_stale_bytecode(repo_root, new_head, restore_head)
    if _is_head_ref_lock_race(update):
        raise SpiceError(
            _head_ref_lock_race_recovery(
                label,
                expected_head=expected_head,
                current_head=current_head,
                completed=update,
            )
        )
    raise SpiceError(_fail(action, update))


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


def _task_footprint_paths(
    repo_root: Path, upstream_head: str, agent_head: str
) -> set[str]:
    """Paths touched by the lane's own non-merge commits since the baseline.

    Merges are excluded on purpose: an overlap-resolution merge makes the
    baseline an ancestor of the lane head, and its combined diff would
    dissolve the footprint into every path the resolution happened to touch.
    """
    listing = _read(
        repo_root,
        "log",
        "--no-renames",
        "--no-merges",
        "--format=",
        "--name-only",
        f"{upstream_head}..{agent_head}",
    )
    return {line for line in listing.splitlines() if line}


def _refuse_out_of_scope_landing(
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
        for line in _read(
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
    return _run(repo_root, "cat-file", "-e", f"{treeish}:{path}").returncode == 0


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


def _merge_conflict_recovery(
    label: str, repo_root: Path, merged_upstream_head: str
) -> str:
    conflicts = _conflict_paths(repo_root)
    if not _read(repo_root, "rev-parse", "--verify", "MERGE_HEAD"):
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
    tree = _tree_of(repo_root, treeish)
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


def _compose_message(label: str, meta: dict[str, str] | None) -> str:
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


def _fail(action: str, completed: subprocess.CompletedProcess[str]) -> str:
    detail = "\n".join(
        part.strip()
        for part in (completed.stdout, completed.stderr)
        if part and part.strip()
    )
    suffix = f"\n{detail}" if detail else ""
    return f"could not {action} (git exit {completed.returncode}){suffix}"
