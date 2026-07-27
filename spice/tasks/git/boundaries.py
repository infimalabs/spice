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
* **agent launch** (`fast_forward_if_safe`): opportunistically fast-forward a
  clean, uncommitted lane immediately before its supervisor or native harness
  starts — the very same safe advance activation applies from inside the
  checkout, so both pre-run refreshes share one code path. It never raises:
  dirty, locally committed, divergent, or unfetchable lanes keep their work
  exactly as-is and start with an explicit skip note. A lane is thus never
  locked out — nor its tree mangled or reset — by its own checkout or an
  unavailable remote; the agent it starts reconciles whatever the advance skips.

The default baseline is the current branch's user-managed merge target on the
conventional ``origin`` remote, or ``origin/HEAD`` when no merge is configured.
When no remote exists (local-only trees, or test harnesses) every operation
degrades to a safe no-op that still records the local HEAD, so the captured
review record holds without a remote.

This is the top of `spice.tasks.git` and owns the boundaries themselves — *when*
Git is touched and what each one records. Below it, `merging` carries a single
integration through and writes the refusal an agent reads when it cannot finish,
and `plumbing` runs every Git command either of them issues. Both are called by
module name, so a test that patches one attribute is seen by every caller above
it.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from spice.errors import SpiceError
from spice.tasks import config, wordingreview
from spice.tasks.git import merging, plumbing

# Publish-race rounds before surfacing recovery guidance: enough for a full
# N-agent completion storm to drain ahead of this push, small enough that a
# genuinely wedged remote fails fast.
PUBLISH_RACE_RETRY_LIMIT = 5
CHECKOUT_PACKAGED_SKILL_RELATIVE_PATH = Path("spice") / "agent" / "SKILL.md"


class MergeConflict(SpiceError):
    """A real content conflict the agent must resolve before the phase closes."""


@dataclass
class SyncResult:
    notes: list[str] = field(default_factory=list)
    uda_args: list[str] = field(default_factory=list)


def _resolve_target(repo_root: Path) -> tuple[str, str] | None:
    """Return ``(remote, baseline_ref)`` for this worktree's task baseline,
    or ``None`` when the configured remote is absent (local-only).

    The current branch's configured merge is authoritative. Missing merge
    config falls back to ``origin/HEAD`` in remote-backed worktrees.
    """
    upstream = branch_upstream_target(repo_root)
    if upstream is not None:
        return upstream
    if not plumbing.read(repo_root, "remote"):
        return None
    raise SpiceError(
        "add an origin remote, configure branch tracking, or use a local-only tree; "
        "cannot resolve task baseline: origin remote is unavailable"
    )


def branch_upstream_target(repo_root: Path) -> tuple[str, str] | None:
    # The lane's user-managed merge (branch.<lane>.merge) is the single source of
    # truth — and it stays readable under the agent shadow: the shadow's
    # self-merge lives in *system* scope, so `git config --get` returns the
    # native value (worktree or common config). The remote is `origin`
    # by convention (branch.<lane>.remote is poisoned to `.` by the shadow's
    # command-scope pair, so it cannot be trusted). origin/HEAD is only a
    # backstop when the lane has no tracking configured.
    if plumbing.run(repo_root, "remote", "get-url", "origin").returncode != 0:
        return None
    branch = plumbing.read(repo_root, "symbolic-ref", "--quiet", "--short", "HEAD")
    prefix = "refs/heads/"
    merge = (
        plumbing.read(repo_root, "config", "--get", f"branch.{branch}.merge")
        if branch
        else ""
    )
    if merge.startswith(prefix):
        return "origin", f"origin/{merge[len(prefix) :]}"
    return _origin_head_backstop_target(repo_root)


def _origin_head_backstop_target(repo_root: Path) -> tuple[str, str]:
    head_ref = plumbing.read(repo_root, "symbolic-ref", "refs/remotes/origin/HEAD")
    prefix = "refs/remotes/"
    if not head_ref.startswith(prefix):
        raise SpiceError(
            "run `git remote set-head origin --auto` or configure branch tracking so "
            "the task baseline can resolve the integration branch; the lane has no "
            "branch.<lane>.merge and origin/HEAD is unset"
        )
    return "origin", head_ref[len(prefix) :]


def _parents(repo_root: Path, commit: str) -> list[str]:
    line = plumbing.read(repo_root, "rev-list", "--parents", "-n", "1", commit)
    parts = line.split()
    return parts[1:]


def _is_merge_with_first_parent(repo_root: Path, commit: str, parent: str) -> bool:
    """True when ``commit`` is a merge with ``parent`` as its mainline."""
    parents = _parents(repo_root, commit)
    return len(parents) >= 2 and parents[0] == parent


def _worktree_dirty(repo_root: Path) -> bool:
    return plumbing.read(repo_root, "status", "--porcelain") != ""


def _refresh_generated_skill_after_advance(repo_root: Path) -> str | None:
    """Rematerialize the ignored worktree skill from the newly advanced tree.

    A Spice source checkout owns the packaged skill that just arrived through
    the fast-forward. Other repositories keep using the installed package's
    source. Materialization intentionally preserves a tracked worktree skill;
    ignored copies must match byte-for-byte or the sync reports the failure.

    Every failure leaves as a note, because every caller runs this only after
    HEAD has already advanced. The arriving bytes are whatever the advanced
    tree carries, so a skill that is not valid UTF-8 is reachable and decodes
    to a note rather than an exception that would wedge every lane's launch.
    The landing caller raises the stakes further: it runs after the merge has
    published, where an escaping exception would fail work already pushed.
    """
    from spice.agent import lifecyclebinding

    checkout_source = repo_root / CHECKOUT_PACKAGED_SKILL_RELATIVE_PATH
    packaged = (
        checkout_source
        if checkout_source.is_file()
        else lifecyclebinding.packaged_skill_path()
    )
    try:
        target = lifecyclebinding.materialize_worktree_skill(
            repo_root, packaged_path=packaged
        )
        if lifecyclebinding.git_tracks_relative_path(
            repo_root, lifecyclebinding.WORKTREE_SKILL_RELATIVE_PATH
        ):
            return None
        if target is None or target.read_bytes() != packaged.read_bytes():
            return (
                "generated skill refresh failed: "
                f"{lifecyclebinding.WORKTREE_SKILL_RELATIVE_PATH.as_posix()} "
                "does not match its packaged source"
            )
    except (OSError, UnicodeDecodeError) as exc:
        return f"generated skill refresh failed: {exc}"
    return None


def _ahead_behind(repo_root: Path, baseline: str) -> tuple[int, int]:
    completed = plumbing.run(
        repo_root, "rev-list", "--left-right", "--count", f"{baseline}...HEAD"
    )
    try:
        behind_text, ahead_text = completed.stdout.split()
        behind, ahead = int(behind_text), int(ahead_text)
    except (AttributeError, TypeError, ValueError):
        behind = ahead = -1
    if completed.returncode != 0 or behind < 0 or ahead < 0:
        raise SpiceError(
            f"the relationship to {baseline} could not be inspected\n"
            + plumbing.fail(f"inspect relationship to {baseline}", completed)
        )
    return behind, ahead


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
    ahead = plumbing.read(repo_root, "rev-list", "--count", f"{baseline}..HEAD")
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
            "commit or clear the working tree first; cannot start new work"
        )
    ahead = plumbing.read(root, "rev-list", "--count", f"{baseline}..HEAD")
    if ahead and ahead != "0":
        raise SpiceError(
            # "them" led the tail once the repair moved ahead of it, so the
            # pronoun becomes the noun it referred to; nothing else changes.
            "capture or clear the local commits first; cannot start new work: "
            f"the branch has {ahead} local commit(s) not yet recorded by a "
            "completed task"
        )
    before = plumbing.read(root, "rev-parse", "HEAD")
    plumbing.run(root, "fetch", remote)
    if not plumbing.read(root, "rev-parse", baseline):
        raise SpiceError(f"baseline {baseline} not found on remote {remote}")
    completed = plumbing.run(root, "merge", "--ff-only", baseline)
    if completed.returncode != 0:
        raise SpiceError(
            "resolve local git state first; cannot start new work: the working "
            "tree could not be brought to the current baseline cleanly"
        )
    after = plumbing.read(root, "rev-parse", "HEAD")
    blocked = plumbing.purge_stale_bytecode(root, before, after)
    notes = ["updated working tree to the current baseline"] if after != before else []
    if after != before:
        refresh_note = _refresh_generated_skill_after_advance(root)
        if refresh_note is not None:
            notes.append(refresh_note)
    if blocked:
        notes.append(plumbing.bytecode_cleanup_note(blocked))
    return SyncResult(notes=notes)


def fast_forward_if_safe(repo_root: Path | None = None) -> SyncResult:
    """Bring the tree up to the current baseline when, and only when, it is
    safe.

    The single safe advance shared by the two opportunistic pre-run refreshes:
    the control plane's **agent launch** (from the globally installed spice,
    before ``python -m spice`` and the native harness import from the checkout)
    and the agent's own **activation** (from inside the checkout). Lenient
    sibling of :func:`prepare_for_claim`: it applies the same rules (clean tree,
    zero commits ahead, fast-forward-only) but never raises, so a lane always
    starts and its tree is never mangled or reset. Every outcome is reported as
    a note rather than a silent no-op: ``current`` when already up to date,
    or a specific ``skipped:`` note for each safe no-op, so a non-advance is
    observable in the packet instead of invisible. A missing remote, failed
    fetch, and uninspectable baseline remain distinct outcomes.
    """
    root = repo_root or config.repo_root()
    try:
        resolved = _resolve_target(root)
    except SpiceError:
        return SyncResult(notes=["skipped:baseline-uninspectable"])
    if resolved is None:
        return SyncResult(notes=["skipped:no-remote"])
    remote, baseline = resolved
    if _worktree_dirty(root):
        return SyncResult(notes=["skipped:dirty"])
    fetched = plumbing.run(root, "fetch", remote)
    if fetched.returncode != 0:
        return SyncResult(notes=["skipped:fetch-failed"])
    if not plumbing.read(root, "rev-parse", baseline):
        return SyncResult(notes=["skipped:baseline-uninspectable"])
    try:
        behind, ahead = _ahead_behind(root, baseline)
    except SpiceError:
        return SyncResult(notes=["skipped:baseline-uninspectable"])
    if ahead:
        return SyncResult(notes=["skipped:diverged" if behind else "skipped:ahead"])
    before = plumbing.read(root, "rev-parse", "HEAD")
    if plumbing.run(root, "merge", "--ff-only", baseline).returncode != 0:
        return SyncResult(notes=["skipped:diverged"])
    after = plumbing.read(root, "rev-parse", "HEAD")
    blocked = plumbing.purge_stale_bytecode(root, before, after)
    notes = [
        "updated working tree to the current baseline" if after != before else "current"
    ]
    if after != before:
        refresh_note = _refresh_generated_skill_after_advance(root)
        if refresh_note is not None:
            notes.append(refresh_note)
    if blocked:
        notes.append(plumbing.bytecode_cleanup_note(blocked))
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
    diff changes baseline paths outside the task's own commits, as is a
    landing that touches a declared suite seam while the whole suite is red
    on the merged tree. With no
    configured remote this records the local HEAD and performs no network or
    history mutation.

    A landing that moves HEAD carries the baseline's packaged skill into this
    tree, so it rematerializes the ignored generated copy exactly as the claim
    and launch advances do. This is the third HEAD-moving boundary; without it
    new packaged bytes arrive here and the next claim correctly observes no
    advance and correctly skips, leaving the stale copy in service for the
    lifetime of the lane.
    """
    root = repo_root or config.repo_root()
    wordingreview.require_integrate_allowed(label, meta)
    agent_head = plumbing.read(root, "rev-parse", "HEAD")
    resolved = _resolve_target(root)
    local_commits = _commits_ahead_of_target(root, resolved)
    if resolved is None:
        return SyncResult(
            uda_args=merging.capture(agent_head, agent_head, "", "", local_commits)
        )
    remote, baseline = resolved

    upstream_head = _fetch_upstream_head(root, remote, baseline)
    if agent_head == upstream_head:
        # Nothing to integrate; the baseline already holds this state.
        return SyncResult(
            uda_args=merging.capture(
                agent_head,
                agent_head,
                baseline,
                upstream_head,
                local_commits,
            )
        )

    message = merging.compose_message(label, meta)
    merge_head = _integrate_task_work(
        root,
        baseline=baseline,
        label=label,
        agent_head=agent_head,
        upstream_head=upstream_head,
        message=message,
    )
    tree_already_integrated = merging.tree_of(root, merge_head) == merging.tree_of(
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
    notes = [tree_same_note] if tree_same_note else []
    if plumbing.read(root, "rev-parse", "HEAD") != agent_head:
        refresh_note = _refresh_generated_skill_after_advance(root)
        if refresh_note is not None:
            notes.append(refresh_note)
    return SyncResult(
        notes=notes,
        uda_args=merging.capture(
            agent_head,
            merge_head,
            baseline,
            upstream_head,
            local_commits,
        ),
    )


def _fetch_upstream_head(repo_root: Path, remote: str, baseline: str) -> str:
    plumbing.run(repo_root, "fetch", remote)
    upstream_head = plumbing.read(repo_root, "rev-parse", baseline)
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
    if plumbing.is_ancestor(repo_root, upstream_head, "HEAD"):
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
    return merging.synthesize_and_fast_forward(
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
    if plumbing.read(repo_root, "rev-parse", "--verify", "MERGE_HEAD"):
        raise MergeConflict(
            merging.merge_conflict_recovery(label, repo_root, upstream_head)
        )
    merge = plumbing.run(
        repo_root,
        "merge-tree",
        "--write-tree",
        "-z",
        "--no-messages",
        agent_head,
        upstream_head,
    )
    merged_tree, conflict_records = merging.parse_merge_tree_output(merge.stdout)
    if merge.returncode == 1:
        merging.materialize_merge_conflict(
            repo_root,
            merged_tree=merged_tree,
            conflict_records=conflict_records,
            agent_head=agent_head,
            upstream_head=upstream_head,
            message=message,
        )
        raise MergeConflict(
            merging.merge_conflict_recovery(label, repo_root, upstream_head)
        )
    if merge.returncode != 0 or not merged_tree:
        raise SpiceError(plumbing.fail("compute task merge tree", merge))
    return merging.synthesize_and_fast_forward(
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
    flagged = merging.conflict_marker_paths(repo_root, upstream_head, merge_head)
    if flagged:
        raise SpiceError(merging.conflict_marker_refusal(label, flagged))
    merging.refuse_out_of_scope_landing(
        repo_root,
        label=label,
        upstream_head=upstream_head,
        merge_head=merge_head,
        agent_head=agent_head,
    )
    merging.refuse_red_suite_landing(
        repo_root, label=label, upstream_head=upstream_head, agent_head=agent_head
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
    push = plumbing.run(repo_root, "push", remote, f"{merge_head}:{branch}")
    if push.returncode == 0:
        return merge_head, upstream_head
    if not _is_non_fast_forward_push(push):
        raise SpiceError(plumbing.fail(f"publish task work to {baseline}", push))
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
        fetch = plumbing.run(repo_root, "fetch", remote)
        if fetch.returncode != 0:
            raise SpiceError(
                merging.publish_race_recovery(label, remote, baseline, last_push)
            )
        fresh_upstream_head = plumbing.read(repo_root, "rev-parse", baseline)
        if not fresh_upstream_head or fresh_upstream_head == previous_upstream_head:
            raise SpiceError(
                merging.publish_race_recovery(label, remote, baseline, last_push)
            )
        if fresh_upstream_head == merge_head:
            return merge_head, fresh_upstream_head

        merge = plumbing.run(
            repo_root,
            "merge-tree",
            "--write-tree",
            "-z",
            "--no-messages",
            merge_head,
            fresh_upstream_head,
        )
        merged_tree, conflict_records = merging.parse_merge_tree_output(merge.stdout)
        if merge.returncode == 1:
            merging.materialize_merge_conflict(
                repo_root,
                merged_tree=merged_tree,
                conflict_records=conflict_records,
                agent_head=merge_head,
                upstream_head=fresh_upstream_head,
                message=message,
            )
            raise MergeConflict(
                merging.merge_conflict_recovery(label, repo_root, fresh_upstream_head)
            )
        if merge.returncode != 0 or not merged_tree:
            raise SpiceError(plumbing.fail("compute publish-race merge tree", merge))
        retry_head = merging.synthesize_and_fast_forward(
            repo_root,
            merged_tree,
            fresh_upstream_head,
            merge_head,
            message,
            label=label,
        )
        flagged = merging.conflict_marker_paths(
            repo_root, fresh_upstream_head, retry_head
        )
        if flagged:
            raise SpiceError(merging.conflict_marker_refusal(label, flagged))
        merging.refuse_out_of_scope_landing(
            repo_root,
            label=label,
            upstream_head=fresh_upstream_head,
            merge_head=retry_head,
            agent_head=agent_head,
        )
        # The race merged a peer's work into this landing, so the tree that was
        # green a moment ago is not the tree about to be pushed.
        merging.refuse_red_suite_landing(
            repo_root,
            label=label,
            upstream_head=fresh_upstream_head,
            agent_head=agent_head,
        )
        retry_push = plumbing.run(repo_root, "push", remote, f"{retry_head}:{branch}")
        if retry_push.returncode == 0:
            return retry_head, fresh_upstream_head
        if not _is_non_fast_forward_push(retry_push):
            raise SpiceError(
                plumbing.fail(f"publish task work to {baseline}", retry_push)
            )
        previous_upstream_head = fresh_upstream_head
        merge_head = retry_head
        last_push = retry_push
    raise SpiceError(merging.publish_race_recovery(label, remote, baseline, last_push))


def _is_non_fast_forward_push(completed: subprocess.CompletedProcess[str]) -> bool:
    output = (completed.stdout + "\n" + completed.stderr).lower()
    return (
        "non-fast-forward" in output
        or "fetch first" in output
        or "stale info" in output
    )
