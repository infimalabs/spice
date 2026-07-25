"""Claim state, phase slots, and guard rails shared by task mutations.

Leaf module: ops (and anything else) imports from here; nothing here
imports ops, so guards stay usable from any task surface without cycles.

Refusal messages here are repair-first, like every other refusal in the
tree: see `spice.errors` for the rule and its exemptions. This module holds
the densest concentration of them because it is where an agent mid-claim
gets stopped, so it is the surface where leading with the command pays most.
"""

from __future__ import annotations

import json
import math
import os
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from spice.agent.identity import ambient_thread, uuid_thread_id
from spice.agent.paths import agent_thread_state_dir
from spice.agent.sidechannelnotify import (
    SIDE_CHANNEL_CLAIM_EVENT,
    notify_agent_side_channel,
)
from spice.errors import SpiceError
from spice.paths import atomic_write_json
from spice.tasks import config, identity, readiness, tw
from spice.tasks.git import boundaries

CLAIM_WITNESS_FILE = "claim-witness.json"


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


def phases_of(row: Mapping[str, Any]) -> list[str]:
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
    return _validated_claim_meta(
        [
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
    )


def _validated_claim_meta(args: list[str]) -> list[str]:
    registered = config.uda_schema()
    for arg in args:
        key, separator, _value = arg.partition(":")
        if separator and key in registered:
            continue
        raise SpiceError(f"claim metadata key {key!r} is not a registered UDA")
    return args


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
    if not raw:
        raise SpiceError(
            _claim_lease_repair_message(
                row, "active claim has no recorded lease duration"
            )
        )
    try:
        return _resolved_claim_lease_seconds(float(raw))
    except (SpiceError, ValueError) as exc:
        raise SpiceError(
            _claim_lease_repair_message(
                row, f"active claim has unreadable lease duration {raw!r}"
            )
        ) from exc


def _claim_lease_repair_message(row: dict[str, Any], diagnostic: str) -> str:
    handle = identity.render_handle(row)
    suggested_lease = _resolved_claim_lease_seconds(None)
    return (
        f"run `spice task reclaim {handle} --lease-seconds {suggested_lease:g}` "
        f"to repair the claim; {diagnostic}"
    )


def _effective_claim_lease_seconds(
    row: dict[str, Any], requested_lease_seconds: float | None
) -> float:
    """Keep a readable policy, or explicitly replace one that cannot be read."""
    requested = (
        _resolved_claim_lease_seconds(requested_lease_seconds)
        if requested_lease_seconds is not None
        else None
    )
    try:
        recorded = _row_claim_lease_seconds(row)
    except SpiceError:
        if requested is None:
            raise
        return requested
    if requested is None:
        return recorded
    return max(recorded, requested)


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
        f"run `spice task capture --project {project} --origin task:{handle} "
        '--done --validation "..."` to capture already-committed work into a new '
        "task, or discard local work or hand off the current state before "
        f"continuing; cannot {action} a deleted task: {handle}, and the deleted "
        "handle itself cannot be captured."
    )


def _claimed_task_capture_recovery_message(row: dict[str, Any], owner: str) -> str:
    handle = identity.render_handle(row)
    project = str(row.get("project") or "").strip() or "<project>"
    return (
        f"run `spice task capture --project {project} --origin task:{handle} "
        '--done --validation "..."` to capture already-committed work into a new '
        "task, or discard local work or hand off the current state before "
        f"continuing; cannot capture {handle}: task already claimed by {owner}."
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
class ClaimReleaseResult:
    """Whether the row went back on the board, and what the witness cost.

    The two are separate answers because they land at separate moments, and a
    caller reporting a release needs the one the allocator can see.
    """

    released: bool
    witness_error: str = ""


@dataclass(frozen=True)
class ClaimWitness:
    active: bool
    actor: str
    uuid: str
    handle: str = ""


def claim_witness_path(repo_root: Path, actor: str) -> Path | None:
    thread_id = uuid_thread_id(actor)
    if not thread_id:
        return None
    return agent_thread_state_dir(repo_root, thread_id) / CLAIM_WITNESS_FILE


def read_claim_witness(repo_root: Path, actor: str) -> ClaimWitness | None:
    """Read the exact row this thread most recently claimed or retired."""
    path = claim_witness_path(repo_root, actor)
    if path is None:
        return None
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, json.JSONDecodeError) as exc:
        raise SpiceError(f"cannot read claim witness {path}: {exc}") from exc
    if not isinstance(loaded, dict):
        raise SpiceError(f"invalid claim witness {path}: expected an object")
    witness_actor = uuid_thread_id(str(loaded.get("actor") or ""))
    uuid = str(loaded.get("uuid") or "").strip()
    active = loaded.get("active")
    handle = str(loaded.get("handle") or "").strip()
    if (
        witness_actor != uuid_thread_id(actor)
        or not uuid
        or not isinstance(active, bool)
    ):
        raise SpiceError(f"invalid claim witness {path}: missing identity fields")
    if active and not handle:
        raise SpiceError(f"invalid claim witness {path}: active row has no handle")
    return ClaimWitness(active=active, actor=witness_actor, uuid=uuid, handle=handle)


def _write_claim_witness(
    repo_root: Path,
    actor: str,
    *,
    uuid: str,
    handle: str,
    active: bool,
) -> bool:
    try:
        path = claim_witness_path(repo_root, actor)
    except SpiceError:
        # Explicit claim-site metadata may name a worktree that has not been
        # materialized yet. No supervisor can inhabit that path, so retain the
        # established cross-worktree claim behavior without inventing state in
        # the caller's different lane.
        return False
    if path is None:
        return False
    intended = ClaimWitness(
        active=active,
        actor=uuid_thread_id(actor),
        uuid=uuid,
        handle=handle if active else "",
    )
    try:
        if read_claim_witness(repo_root, actor) == intended:
            return False
    except SpiceError:
        # The atomic rewrite repairs an interrupted or manually damaged record.
        pass
    atomic_write_json(
        path,
        {
            "active": intended.active,
            "actor": intended.actor,
            "handle": intended.handle,
            "uuid": intended.uuid,
        },
        compact=True,
        sort_keys=True,
    )
    notify_agent_side_channel(repo_root, event=SIDE_CHANNEL_CLAIM_EVENT)
    return True


def record_claim_witness(uuid: str, actor: str, *, site: ClaimSite) -> bool:
    rows = tw.export([uuid])
    if len(rows) != 1:
        raise SpiceError(f"cannot record claim witness: task UUID {uuid} is not unique")
    fresh = rows[0]
    return _write_claim_witness(
        site.worktree,
        actor,
        uuid=identity.uuid_of(fresh),
        handle=identity.render_handle(fresh),
        active=True,
    )


def retire_claim_witness(
    repo_root: Path, actor: str, *, uuid: str, handle: str = ""
) -> bool:
    """Durably distinguish an intentional/announced end from no history."""
    return _write_claim_witness(
        repo_root,
        actor,
        uuid=uuid,
        handle=handle,
        active=False,
    )


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


CLAIM_RENEWAL_FAILED_REASONS = frozenset({"backend_error", "refused"})


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
            f"run `spice task claim {handle}` to repair the claim; "
            f"{action} requires native ACTIVE state on {handle}"
        )
    if active and not owner:
        raise SpiceError(
            f"run `spice task claim {handle} --steal` to repair ownership; "
            f"{action} blocked: {handle} is ACTIVE but has no claim_by"
        )
    if owner:
        raise SpiceError(f"task claimed by {owner}; not yours to {action}")
    raise SpiceError(
        f"run `spice task next` (or `spice task claim {handle}`) first; "
        f"{action} requires a claim"
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
        "run `spice task next` for work you can claim, and leave this row for "
        f"another actor; cannot manually claim {handle}: this thread authored "
        "the review"
    )


def _export_active() -> list[dict[str, Any]]:
    # Bare +ACTIVE: a claimed deferred task keeps its wait, and the
    # status:pending filter synthesizes `waiting` for future-wait rows, which
    # would hide the claim from its own owner.
    return tw.export(["+ACTIVE"])


def _claims_by_actor(rows: list[dict[str, Any]], actor: str) -> list[dict[str, Any]]:
    return [r for r in rows if str(r.get("claim_by") or "") == actor]


def _latest_claim(claims: list[dict[str, Any]]) -> dict[str, Any] | None:
    if not claims:
        return None
    return max(claims, key=lambda r: str(r.get("claim_at") or ""))


def _active_claims_for(actor: str) -> list[dict[str, Any]]:
    return _claims_by_actor(_export_active(), actor)


def has_active_claim() -> bool:
    """Whether the current actor holds an active task claim."""
    return bool(_active_claims_for(tw.current_actor()))


def active_claim(actor: str) -> dict[str, Any] | None:
    """The actor's active task claim (latest claim_at), or None."""
    if not actor:
        return None
    return _latest_claim(_active_claims_for(actor))


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
        "add child tasks with acceptance and native dependencies, then run "
        "`spice task done` with a clean tree and zero local implementation "
        "commits, and claim an implementation child task before creating, "
        f"capturing, or landing code; {action} blocked: {handle} is in plan "
        f"phase, whose output is board state.{suffix}"
    )


def _require_plan_phase_done_has_no_local_commits(row: dict[str, Any]) -> None:
    if str(row.get("phase") or "") != "plan":
        return
    ahead = boundaries.commits_ahead_of_baseline()
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
            f"run `spice task unclaim {active_handle}` (or complete it) before "
            f"claiming {target_handle}; {action} would create multiple active "
            f"claims for {actor}"
        )
    raise SpiceError(
        f"run `spice task unclaim {active_handle}` (or complete it) before "
        f"claiming new work; {action} would create multiple active claims "
        f"for {actor}"
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
    metadata = claim_meta(
        actor,
        site=site,
        context_thread=context_thread,
        lease_seconds=lease_seconds,
    )
    try:
        tw.run(
            [
                uuid,
                *filters,
                "modify",
                *metadata,
                f"{config.TASK_READY_AT_UDA}:",
                "start:now",
            ]
        )
    except SpiceError:
        if guard_unclaimed:
            return False
        raise
    record_claim_witness(uuid, actor, site=site)
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
    metadata = claim_meta(
        actor,
        site=site,
        context_thread=context_thread,
        lease_seconds=lease_seconds,
    )
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
                *metadata,
                "start:now",
            ]
        )
    except SpiceError:
        return False
    record_claim_witness(uuid, actor, site=site)
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
    record_claim_witness(uuid, next_actor, site=site)
    retire_claim_witness(site.worktree, prior_actor, uuid=uuid, handle=handle)
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


def release_claim(uuid: str, actor: str) -> ClaimReleaseResult:
    """Release only the exact active claim still owned by ``actor``.

    The modify is the release: it clears the claim and stamps the row's new
    READY transition in one write, so the moment it lands the task is
    allocatable again and no later failure can take that back. Retiring the
    witness is a second write to a second place, so a filesystem fault there is
    reported beside the release rather than raised over it -- raising would tell
    every caller the row is still reserved when it is already back on the board,
    and a launch handing back a reservation it never held is exactly the report
    an operator chases.
    """
    released_at = tw.now_iso()
    ready_after_release = readiness.ready_when_inactive(uuid)
    claim_actor = tw.canonical_actor(actor or config.SENTINEL_ACTOR)
    rows = tw.export([uuid])
    claim_worktree = (
        Path(str(rows[0].get("claim_worktree") or config.repo_root()))
        if len(rows) == 1
        else config.repo_root()
    )
    try:
        tw.run(
            [
                uuid,
                "+ACTIVE",
                f"claim_by.is:{claim_actor}",
                "modify",
                "start:",
                *CLAIM_CLEAR,
                readiness.transition_arg(
                    at=released_at,
                    ready=ready_after_release,
                ),
            ]
        )
    except SpiceError:
        return ClaimReleaseResult(released=False)
    try:
        retire_claim_witness(claim_worktree, claim_actor, uuid=uuid)
    except OSError as exc:
        # The witness is a file: `atomic_write_json` is the only leaf here that
        # touches anything but memory, and its side-channel notify already
        # swallows its own socket errors.
        return ClaimReleaseResult(released=True, witness_error=str(exc))
    return ClaimReleaseResult(released=True)


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
    metadata = _renewal_claim_meta(
        resolved_actor,
        site=site,
        lease_seconds=_effective_claim_lease_seconds(row, lease_seconds),
    )
    try:
        tw.run(
            [
                uuid,
                "+ACTIVE",
                f"claim_by.is:{resolved_actor}",
                f"claim_worktree.is:{site.worktree}",
                "modify",
                *metadata,
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
    record_claim_witness(uuid, resolved_actor, site=site)
    return ClaimRenewalResult(
        True,
        "renewed",
        handle=identity.render_handle(fresh),
        claim_until=str(fresh.get("claim_until") or ""),
        uuid=identity.uuid_of(fresh),
    )


def renew_claim_or_report(actor: str | None = None) -> ClaimRenewalResult:
    """Renew for a surface that has to finish even when the claim will not.

    Renewal already reports every condition it expects -- no claim, a deleted
    or completed task, another worktree's claim, a backend that rejected the
    write. What is left raises, and an unreadable recorded lease is the one an
    agent meets: `spice task next` refuses on it outright, because allocation
    must not proceed against a claim whose policy nobody can read. Activation
    is the opposite case. It is the first command an agent runs and the only
    place the steering key and its authenticity contract are handed over, so
    one unreadable task row must not withhold all of that. The refusal becomes
    a reported line carrying its repair command verbatim instead.
    """
    try:
        return renew_claim(actor=actor)
    except SpiceError as exc:
        return ClaimRenewalResult(False, "refused", detail=str(exc))


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
