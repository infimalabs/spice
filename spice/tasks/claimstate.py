"""Claim state, phase slots, and guard rails shared by task mutations.

Leaf module: ops (and anything else) imports from here; nothing here
imports ops, so guards stay usable from any task surface without cycles.
"""

from __future__ import annotations

import math
import os
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from spice.agent.identity import ambient_thread
from spice.errors import SpiceError
from spice.tasks import config, gitsync, identity, tw


def annotate(target: str, text: str) -> None:
    """Annotate via `-- ` so attribute-like text (e.g. "key: value") stays
    literal."""
    text = _task_text(text)
    tw.run([target, "annotate", "--", text])


def _task_text(text: str) -> str:
    return text


# ---- flow / phase slots -------------------------------------------------


def flow_args(phases: list[str]) -> list[str]:
    args = [f"phase_{i}:{phase}" for i, phase in enumerate(phases)]
    args.append(f"phase:{phases[0]}")
    args.append("phase_i:0")
    return args


def phases_of(row: dict[str, Any]) -> list[str]:
    phases: list[str] = []
    for i in range(config.PHASE_SLOT_COUNT):
        value = str(row.get(f"phase_{i}") or "").strip()
        if not value:
            break
        phases.append(value)
    return phases


def phase_index(row: dict[str, Any]) -> int:
    return int(row.get("phase_i") or 0)


# ``claim_at`` deliberately survives release: it is the row-level fact that
# work has begun, so document apply can never regain ownership of that row.
CLAIM_CLEAR = [
    f"{name}:"
    for name in (
        "claim_by",
        "claim_until",
        "claim_thread",
        "claim_worktree",
        "claim_branch",
        "claim_head",
        "claim_lease_seconds",
        "claim_context_start",
        "claim_context_end",
        "claim_context_link",
        "claim_context_turn",
    )
]


def _iso(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


@dataclass(frozen=True)
class ClaimSite:
    worktree: Path
    branch: str
    head: str


def current_claim_site() -> ClaimSite:
    return ClaimSite(
        worktree=config.repo_root(),
        branch=tw.current_branch(),
        head=tw.claim_head(),
    )


def claim_meta(
    actor: str,
    *,
    site: ClaimSite,
    context_thread: str | None,
    lease_seconds: float | None,
) -> list[str]:
    claim_actor = tw.canonical_actor(actor or config.SENTINEL_ACTOR)
    resolved_lease_seconds = _resolved_claim_lease_seconds(lease_seconds)
    at_dt = datetime.now(UTC)
    at = _iso(at_dt)
    until = _iso(at_dt + timedelta(seconds=resolved_lease_seconds))
    start = _iso(at_dt - timedelta(seconds=config.CLAIM_CONTEXT_SECONDS))
    end = _iso(at_dt + timedelta(seconds=config.CLAIM_CONTEXT_SECONDS))
    explicit_context = tw.canonical_actor(context_thread or "")
    ambient = None if explicit_context else ambient_thread()
    if explicit_context:
        thread = explicit_context
        turn = thread
    elif ambient is None:
        thread = claim_actor
        turn = thread
    else:
        thread, driver = ambient
        # Per-turn granularity is a driver capability. Drivers that cannot see
        # turn ids from the command environment intentionally fall back to the
        # thread id, so claim_context_turn equals claim_thread for them.
        turn = (
            driver.current_turn_id(os.environ) or thread
        ).strip()  # env-policy: allow
    link = f"spice-session://{thread}?start={start}&end={end}"
    return [
        f"claim_by:{claim_actor}",
        f"claim_at:{at}",
        f"claim_until:{until}",
        f"claim_thread:{thread}",
        f"claim_worktree:{site.worktree}",
        f"claim_branch:{site.branch}",
        f"claim_head:{site.head}",
        f"claim_lease_seconds:{resolved_lease_seconds:g}",
        f"claim_context_start:{start}",
        f"claim_context_end:{end}",
        f"claim_context_link:{link}",
        f"claim_context_turn:{turn}",
    ]


def _resolved_claim_lease_seconds(lease_seconds: float | None) -> float:
    resolved = (
        float(config.CLAIM_TTL_SECONDS)
        if lease_seconds is None
        else float(lease_seconds)
    )
    if not math.isfinite(resolved) or resolved <= 0:
        raise SpiceError("claim lease seconds must be positive")
    return resolved


def _row_claim_lease_seconds(row: dict[str, Any]) -> float:
    raw = str(row.get("claim_lease_seconds") or "").strip()
    try:
        return _resolved_claim_lease_seconds(float(raw) if raw else None)
    except (SpiceError, ValueError):
        return _resolved_claim_lease_seconds(None)


def _effective_claim_lease_seconds(
    row: dict[str, Any], requested_lease_seconds: float | None
) -> float:
    """Keep an active claim on its longest recorded or newly requested lease."""
    return max(
        _row_claim_lease_seconds(row),
        _resolved_claim_lease_seconds(requested_lease_seconds),
    )


def _require_pending(row: dict[str, Any], action: str) -> None:
    status = str(row.get("status") or "")
    if status == "deleted":
        raise SpiceError(_deleted_task_recovery_message(row, action))
    if status == "completed":
        raise SpiceError(
            f"cannot {action} a completed task: {identity.render_handle(row)}"
        )


def _deleted_task_recovery_message(row: dict[str, Any], action: str) -> str:
    handle = identity.render_handle(row)
    project = str(row.get("project") or "").strip() or "<project>"
    return (
        f"cannot {action} a deleted task: {handle}. "
        "If deletion invalidated local work, discard local work or hand off the "
        "current state before continuing. If you already committed work, do not "
        "capture the deleted handle; capture into a new task with "
        f"`spice task capture --project {project} --origin task:{handle} "
        '--done --validation "..."`.'
    )


def _claimed_task_capture_recovery_message(row: dict[str, Any], owner: str) -> str:
    handle = identity.render_handle(row)
    project = str(row.get("project") or "").strip() or "<project>"
    return (
        f"cannot capture {handle}: task already claimed by {owner}. "
        "If this is a duplicate or canonical task owned by another agent, discard "
        "local work or hand off the current state before continuing. If you "
        "already committed work, capture into a new task with "
        f"`spice task capture --project {project} --origin task:{handle} "
        '--done --validation "..."`.'
    )


@dataclass(frozen=True)
class LiveClaim:
    claim_by: str
    claim_thread: str
    claim_until: str


@dataclass(frozen=True)
class ClaimRenewalResult:
    renewed: bool
    reason: str
    handle: str = ""
    claim_until: str = ""
    detail: str = ""
    uuid: str = ""


@dataclass(frozen=True)
class ClaimCarryResult:
    carried: bool
    reason: str
    handle: str = ""
    claim_until: str = ""
    uuid: str = ""


def claim_carry_status_line(result: ClaimCarryResult) -> str:
    if result.carried:
        return f"claim_carry=carried {result.handle} until {result.claim_until}"
    return f"claim_carry=skipped {result.reason}"


CLAIM_RENEWAL_FAILED_REASONS = frozenset({"backend_error"})


def claim_renewal_state(result: ClaimRenewalResult) -> str:
    if result.renewed:
        return "renewed"
    if result.reason in CLAIM_RENEWAL_FAILED_REASONS:
        return "failed"
    return "skipped"


def claim_renewal_status_line(result: ClaimRenewalResult) -> str:
    """A concise status line for surfaces that opportunistically renew claims."""
    if result.renewed:
        return f"claim_renewal=renewed {result.handle} until {result.claim_until}"
    parts = [f"claim_renewal={claim_renewal_state(result)}", result.reason]
    if result.handle:
        parts.append(result.handle)
    if result.detail:
        parts.append(f"detail={_compact_claim_renewal_detail(result.detail)}")
    return " ".join(parts)


def _compact_claim_renewal_detail(detail: str) -> str:
    return " ".join(detail.split())


def _live_claim(row: dict[str, Any]) -> LiveClaim | None:
    until = str(row.get("claim_until") or "")
    if not until or until < tw.now_iso():
        return None
    claim_by = str(row.get("claim_by") or "")
    claim_thread = str(row.get("claim_thread") or "")
    if not claim_by and not claim_thread:
        return None
    return LiveClaim(
        claim_by=claim_by or "-",
        claim_thread=claim_thread or "-",
        claim_until=until,
    )


def _live_claim_text(claim: LiveClaim) -> str:
    return (
        f"claim_by={claim.claim_by} claim_thread={claim.claim_thread} "
        f"claim_until={claim.claim_until}"
    )


def _require_owner(row: dict[str, Any], actor: str, action: str) -> None:
    owner = str(row.get("claim_by") or "")
    active = bool(row.get("start"))
    if owner == actor and active:
        return
    handle = identity.render_handle(row)
    if owner == actor:
        raise SpiceError(
            f"{action} requires native ACTIVE state on {handle}; "
            "run `spice task claim <handle>` to repair the claim"
        )
    if active and not owner:
        raise SpiceError(
            f"{action} blocked: {handle} is ACTIVE but has no claim_by; "
            "run `spice task claim <handle> --steal` to repair ownership"
        )
    if owner:
        raise SpiceError(f"task claimed by {owner}; not yours to {action}")
    raise SpiceError(
        f"{action} requires a claim; run `spice task next` (or `task claim`) first"
    )


def is_same_author_review(row: dict[str, Any], actor: str) -> bool:
    """Whether this actor authored the work sitting in the row's review phase.

    `review_author` is the only field that names the author of the reviewed
    work. The creator of the task can be a different lane entirely — one lane
    files a task out of its own run and the allocator hands the todo phase to
    another — so a comparison against the creator passes on exactly the case
    that must be refused.
    """
    return (
        str(row.get("phase") or "") == "review"
        and str(row.get("review_author") or "") == actor
    )


def _require_manual_claim_allowed(row: dict[str, Any], actor: str) -> None:
    if not is_same_author_review(row, actor):
        return
    handle = identity.render_handle(row)
    raise SpiceError(
        f"cannot manually claim {handle}: this thread authored the review; "
        "leave it for another actor"
    )


def _active_claims_for(actor: str) -> list[dict[str, Any]]:
    # Bare +ACTIVE: a claimed deferred task keeps its wait, and the
    # status:pending filter synthesizes `waiting` for future-wait rows, which
    # would hide the claim from its own owner.
    return [r for r in tw.export(["+ACTIVE"]) if str(r.get("claim_by") or "") == actor]


def has_active_claim() -> bool:
    """Whether the current actor holds an active task claim."""
    return bool(_active_claims_for(tw.current_actor()))


def active_claim(actor: str) -> dict[str, Any] | None:
    """The actor's active task claim (latest claim_at), or None."""
    if not actor:
        return None
    claims = _active_claims_for(actor)
    if not claims:
        return None
    return max(claims, key=lambda r: str(r.get("claim_at") or ""))


def active_claim_phase(actor: str) -> str:
    """The phase of `actor`'s active task claim, or "" when none is held."""
    if not actor:
        return ""
    claims = _active_claims_for(actor)
    return str(claims[0].get("phase") or "") if claims else ""


def require_no_active_plan_phase_implementation(action: str) -> None:
    """Refuse implementation work while the actor holds a plan-phase claim."""
    row = active_claim(tw.current_actor())
    if row is None or str(row.get("phase") or "") != "plan":
        return
    _raise_plan_phase_implementation_block(action, row)


def _raise_plan_phase_implementation_block(
    action: str, row: dict[str, Any], detail: str = ""
) -> None:
    handle = identity.render_handle(row)
    suffix = f" {detail}" if detail else ""
    raise SpiceError(
        f"{action} blocked: {handle} is in plan phase.{suffix} "
        "Plan phase output is board state: add child tasks with acceptance and "
        "native dependencies, then run `spice task done` with a clean tree and "
        "zero local implementation commits. Claim an implementation child task "
        "before creating, capturing, or landing code."
    )


def _require_plan_phase_done_has_no_local_commits(row: dict[str, Any]) -> None:
    if str(row.get("phase") or "") != "plan":
        return
    ahead = gitsync.commits_ahead_of_baseline()
    if ahead <= 0:
        return
    noun = "commit" if ahead == 1 else "commits"
    _raise_plan_phase_implementation_block(
        "task done",
        row,
        f"Found {ahead} local {noun} ahead of the task baseline.",
    )


def resolve_claim_target(handle: str | None, *, action: str) -> dict[str, Any]:
    """Resolve an explicit handle, or infer the current actor's sole active claim.

    Subcommands that act on the task you are actively working (`done`, `review`,
    `unclaim`) accept an omitted handle and fill it from the single claim you
    hold. With no claim, or more than one, the handle stays required so the
    target is never guessed.
    """
    if handle and handle.strip():
        return identity.resolve(handle)
    claims = _active_claims_for(tw.current_actor())
    if len(claims) == 1:
        return claims[0]
    if not claims:
        raise SpiceError(
            f"task {action} requires a handle: no active claim to infer one from"
        )
    held = ", ".join(sorted(identity.render_handle(r) for r in claims))
    raise SpiceError(
        f"task {action} requires an explicit handle: you hold "
        f"{len(claims)} active claims ({held})"
    )


def _require_single_active_slot(
    actor: str, *, action: str, target: dict[str, Any] | None = None
) -> None:
    target_uuid = identity.uuid_of(target) if target else ""
    conflicts = [
        r for r in _active_claims_for(actor) if identity.uuid_of(r) != target_uuid
    ]
    if not conflicts:
        return
    active = max(conflicts, key=lambda r: str(r.get("claim_at") or ""))
    active_handle = identity.render_handle(active)
    if target:
        target_handle = identity.render_handle(target)
        raise SpiceError(
            f"{action} would create multiple active claims for {actor}; "
            f"complete or unclaim {active_handle} before claiming {target_handle}"
        )
    raise SpiceError(
        f"{action} would create multiple active claims for {actor}; "
        f"complete or unclaim {active_handle} before claiming new work"
    )


def do_claim(
    uuid: str,
    actor: str,
    *,
    site: ClaimSite,
    context_thread: str | None,
    lease_seconds: float | None,
    guard_unclaimed: bool = True,
) -> bool:
    """Atomic claim: set the `start` date AND the claim metadata in one modify.

    A single locked write means a crash can never leave an active-but-
    unclaimed row (which would be stranded: skipped by `next` yet resumable by
    no one). Idempotent — re-claiming (including a steal of an already-active
    row) just rewrites the owner and refreshes the deadline.

    Claim is a pure ownership-and-lease operation: it never touches `wait`,
    `scheduled`, `due`, `until`, or any other scheduling field, so deferral
    survives phase work. Only explicit wake or scheduling edits change timing.
    """
    filters = (
        ["(", "status:pending", "or", "status:waiting", ")", "-ACTIVE"]
        if guard_unclaimed
        else []
    )
    try:
        tw.run(
            [
                uuid,
                *filters,
                "modify",
                *claim_meta(
                    actor,
                    site=site,
                    context_thread=context_thread,
                    lease_seconds=lease_seconds,
                ),
                "start:now",
            ]
        )
    except SpiceError:
        if guard_unclaimed:
            return False
        raise
    _record_task_lifecycle_event(uuid, "claim", actor)
    return True


def take_over_stale_claim(
    uuid: str,
    actor: str,
    *,
    expected_owner: str,
    expected_until: str,
    site: ClaimSite,
    context_thread: str | None,
    lease_seconds: float | None,
) -> bool:
    """Replace exactly the stale claim observed by the allocator.

    The owner and deadline form a compare-and-swap guard. A concurrent
    allocator that selected the same exported row cannot overwrite the fresh
    owner or lease installed by the winner.
    """
    prior_actor = tw.canonical_actor(expected_owner)
    prior_until = expected_until.strip()
    if not prior_actor or not prior_until:
        return False
    try:
        tw.run(
            [
                uuid,
                "+ACTIVE",
                f"claim_by.is:{prior_actor}",
                f"claim_until.is:{prior_until}",
                "(",
                "status:pending",
                "or",
                "status:waiting",
                ")",
                "modify",
                *claim_meta(
                    actor,
                    site=site,
                    context_thread=context_thread,
                    lease_seconds=lease_seconds,
                ),
                "start:now",
            ]
        )
    except SpiceError:
        return False
    _record_task_lifecycle_event(uuid, "claim", actor)
    return True


def carry_claim(
    predecessor: str,
    successor: str,
    *,
    site: ClaimSite,
) -> ClaimCarryResult:
    """Move one active claim to a renewal successor without restarting it."""
    prior_actor = tw.canonical_actor(predecessor)
    next_actor = tw.canonical_actor(successor)
    if not prior_actor:
        return ClaimCarryResult(False, "no_predecessor")
    if prior_actor == next_actor:
        return ClaimCarryResult(False, "same_actor")

    predecessor_claims = tw.export(["status.any:", f"claim_by.is:{prior_actor}"])
    if not predecessor_claims:
        return ClaimCarryResult(False, "no_predecessor_claim")
    if len(predecessor_claims) != 1:
        raise SpiceError(
            "claim carry requires exactly one predecessor claim; "
            f"{prior_actor} owns {len(predecessor_claims)} rows"
        )

    row = predecessor_claims[0]
    handle = identity.render_handle(row)
    _require_pending(row, "claim carry")
    if not row.get("start"):
        raise SpiceError(f"claim carry requires native ACTIVE state on {handle}")
    _require_single_active_slot(next_actor, action="claim carry", target=row)

    carry_meta = [
        arg
        for arg in claim_meta(
            next_actor,
            site=site,
            context_thread=next_actor,
            lease_seconds=_row_claim_lease_seconds(row),
        )
        if not arg.startswith("claim_at:")
    ]
    uuid = identity.uuid_of(row)
    try:
        tw.run(
            [
                uuid,
                "+ACTIVE",
                f"claim_by.is:{prior_actor}",
                "(",
                "status:pending",
                "or",
                "status:waiting",
                ")",
                "modify",
                *carry_meta,
            ]
        )
    except SpiceError as exc:
        raise SpiceError(
            f"claim carry lost the active predecessor claim on {handle}: {exc}"
        ) from exc

    fresh = identity.resolve(handle)
    if str(fresh.get("claim_by") or "") != next_actor or not fresh.get("start"):
        raise SpiceError(
            f"claim carry did not seat {next_actor} on active task {handle}"
        )
    _record_task_lifecycle_event(uuid, "claim", next_actor)
    return ClaimCarryResult(
        True,
        "carried",
        handle=identity.render_handle(fresh),
        claim_until=str(fresh.get("claim_until") or ""),
        uuid=uuid,
    )


def _renewal_claim_meta(
    actor: str, *, site: ClaimSite, lease_seconds: float
) -> list[str]:
    return [
        arg
        for arg in claim_meta(
            actor,
            site=site,
            context_thread=None,
            lease_seconds=lease_seconds,
        )
        if not arg.startswith(("claim_by:", "claim_at:"))
    ]


def release_claim(uuid: str, actor: str) -> bool:
    """Release only the exact active claim still owned by ``actor``."""
    claim_actor = tw.canonical_actor(actor or config.SENTINEL_ACTOR)
    try:
        tw.run(
            [
                uuid,
                "+ACTIVE",
                f"claim_by.is:{claim_actor}",
                "modify",
                "start:",
                *CLAIM_CLEAR,
            ]
        )
    except SpiceError:
        return False
    return True


def _claim_worktree_matches(row: dict[str, Any], repo_root: Path) -> bool:
    raw = str(row.get("claim_worktree") or "").strip()
    if not raw:
        return False
    try:
        return Path(raw).expanduser().resolve() == repo_root.expanduser().resolve()
    except OSError:
        return False


def _claim_renewal_block(
    row: dict[str, Any], actor: str, *, site: ClaimSite
) -> ClaimRenewalResult | None:
    handle = identity.render_handle(row)
    status = str(row.get("status") or "")
    if status == "deleted":
        return ClaimRenewalResult(False, "deleted", handle=handle)
    if status == "completed":
        return ClaimRenewalResult(False, "completed", handle=handle)
    owner = str(row.get("claim_by") or "")
    if owner and owner != actor:
        return ClaimRenewalResult(
            False,
            "claimed_by_other",
            handle=handle,
            claim_until=str(row.get("claim_until") or ""),
            detail=owner,
        )
    if not owner or not row.get("start"):
        return ClaimRenewalResult(False, "no_active_claim", handle=handle)
    if not _claim_worktree_matches(row, site.worktree):
        return ClaimRenewalResult(False, "different_worktree", handle=handle)
    return None


def _claim_renewal_missing_result(
    handle: str | None, exc: SpiceError
) -> ClaimRenewalResult:
    detail = str(exc)
    if detail.startswith("unknown task:"):
        return ClaimRenewalResult(
            False, "missing", handle=(handle or ""), detail=detail
        )
    return ClaimRenewalResult(
        False, "backend_error", handle=(handle or ""), detail=detail
    )


def renew_claim(
    handle: str | None = None,
    *,
    actor: str | None = None,
    lease_seconds: float | None = None,
) -> ClaimRenewalResult:
    """Refresh the current actor/worktree's existing active claim.

    Renewal deliberately is not a claim operation: it never starts an unclaimed
    task, steals a peer claim, repairs ownership, or advances phase state. Its
    effective lease is monotonic: a longer request promotes the recorded claim
    policy, while a shorter request can refresh but never shorten that policy.
    """
    resolved_actor = tw.canonical_actor(actor or tw.current_actor())
    try:
        row = identity.resolve(handle) if handle else active_claim(resolved_actor)
    except SpiceError as exc:
        return _claim_renewal_missing_result(handle, exc)
    if row is None:
        return ClaimRenewalResult(False, "no_active_claim")
    site = current_claim_site()
    blocked = _claim_renewal_block(row, resolved_actor, site=site)
    if blocked is not None:
        return blocked
    uuid = identity.uuid_of(row)
    handle_text = identity.render_handle(row)
    try:
        tw.run(
            [
                uuid,
                "+ACTIVE",
                f"claim_by.is:{resolved_actor}",
                f"claim_worktree.is:{site.worktree}",
                "modify",
                *_renewal_claim_meta(
                    resolved_actor,
                    site=site,
                    lease_seconds=_effective_claim_lease_seconds(row, lease_seconds),
                ),
            ]
        )
    except SpiceError as exc:
        try:
            fresh = identity.resolve(handle_text)
        except SpiceError as resolve_exc:
            return _claim_renewal_missing_result(handle_text, resolve_exc)
        blocked = _claim_renewal_block(fresh, resolved_actor, site=site)
        if blocked is not None:
            return blocked
        return ClaimRenewalResult(
            False, "backend_error", handle=handle_text, detail=str(exc)
        )
    try:
        fresh = identity.resolve(handle_text)
    except SpiceError as exc:
        return _claim_renewal_missing_result(handle_text, exc)
    return ClaimRenewalResult(
        True,
        "renewed",
        handle=identity.render_handle(fresh),
        claim_until=str(fresh.get("claim_until") or ""),
        uuid=identity.uuid_of(fresh),
    )


def _record_task_lifecycle_event(task_id: str, kind: str, actor: str) -> None:
    from spice.serve.team.store import ServeTeamStore
    from spice.tasks import lanes

    agent_id = lanes.route_actor_id(actor or tw.current_actor())
    ServeTeamStore().record_task_lifecycle_event(
        kind,
        task_id=task_id,
        agent_id=agent_id,
    )


def _task_continuation_contract(actor: str | None = None):
    from spice.tasks import lanes

    actor = actor or tw.current_actor()
    route = lanes.team_route_for_actor(actor)
    return lanes.task_continuation_contract(route)
