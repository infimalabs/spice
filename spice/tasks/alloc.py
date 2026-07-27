"""Allocator policy for `task next`: per-agent urgency, stickiness, anti-affinity.

Native urgency ranks first (computed by Taskwarrior under the actor's rc
overrides — anti-self-review plus any lane overlay). Within the top urgency
band, `task next` avoids cells a peer is actively on (spread) and prefers the
smallest move from the actor's last cell (stick).

A review the actor authored is not excluded from the candidate set; the
`ANTI_SELF_REVIEW` coefficient drops its urgency far below ordinary work, so
the actor is handed their own review only as a last resort — when a quiet board
holds nothing else they can take.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.mail.inbox import (
    compose_inbox_text,
    default_inbox_name,
    inbox_item_key,
    write_inbox_item,
)
from spice.sqliteconnection import sqlite_connection
from spice.tasks import claimstate, config, identity, lanes, tw
from spice.tasks.git import boundaries

ANTI_SELF_REVIEW = -100.0  # make self-authored reviews lose to ordinary work
BAND_WIDTH = 5.0  # urgency window treated as "top band" for tie-breaks


@dataclass(frozen=True)
class BriefingTaskSnapshot:
    rows: tuple[dict[str, Any], ...]
    visible_uuids: frozenset[str]


def actor_overrides(actor: str, route: dict[str, Any] | None) -> list[str]:
    return [
        f"rc.urgency.uda.review_author.{actor}.coefficient={ANTI_SELF_REVIEW}",
        *lanes.rc_overrides(route),
    ]


def _cell(row: dict[str, Any]) -> tuple[str, str]:
    return (str(row.get("project") or ""), str(row.get("phase") or ""))


def last_cell(claimed_rows: list[dict[str, Any]]) -> tuple[str, str] | None:
    dated = [r for r in claimed_rows if str(r.get("claim_at") or "")]
    if not dated:
        return None
    latest = max(dated, key=lambda r: str(r.get("claim_at")))
    return _cell(latest)


def peer_cells(actor: str, active_rows: list[dict[str, Any]]) -> set[tuple[str, str]]:
    return {
        _cell(r)
        for r in active_rows
        if str(r.get("claim_by") or "") and str(r.get("claim_by")) != actor
    }


def move_cost(row: dict[str, Any], ref: tuple[str, str] | None) -> int:
    if ref is None:
        return 0
    project, phase = _cell(row)
    return int(project != ref[0]) + int(phase != ref[1])


def _urgency(row: dict[str, Any]) -> float:
    return float(row.get("urgency") or 0.0)


def order(
    ready: list[dict[str, Any]],
    actor: str,
    claimed_rows: list[dict[str, Any]],
    active_rows: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    """Rank candidates best-first. Native urgency first (the top band), then
    within the band spread off cells a peer is active on, then stick to the
    smallest move from the actor's last cell. `next` walks this order,
    claiming until one claim verifies — so a lost race just falls through to
    the next."""
    ref = last_cell(claimed_rows)
    crowded = peer_cells(actor, active_rows)
    top = max(_urgency(r) for r in ready)

    def key(r: dict[str, Any]) -> tuple[int, bool, int, float]:
        in_band = _urgency(r) >= top - BAND_WIDTH
        return (
            0 if in_band else 1,
            (_cell(r) in crowded) if in_band else False,
            move_cost(r, ref) if in_band else 0,
            -_urgency(r),
        )

    return sorted(ready, key=key)


def is_oops(row: dict[str, Any]) -> bool:
    return config.is_oops_project(str(row.get("project") or ""))


def is_hidden(row: dict[str, Any]) -> bool:
    return config.is_hidden_project(str(row.get("project") or ""))


def oops_rows() -> list[dict[str, Any]]:
    """Deferred oops items carry a far-future wait, so they are `waiting`."""
    return [
        r
        for r in tw.export([f"project:{config.OOPS_PROJECT}"])
        if str(r.get("status")) in ("pending", "waiting") and is_oops(r)
    ]


def stale_rows() -> list[dict[str, Any]]:
    """Active claims whose deadline has elapsed (claim_until < now). ISO-8601
    timestamps share a format here, so a lexicographic compare is
    chronological."""
    now = tw.now_iso()
    out: list[dict[str, Any]] = []
    for r in tw.export(["+ACTIVE"]):
        if not _is_open_task(r):
            continue
        until = str(r.get("claim_until") or "")
        if until and until < now:
            out.append(r)
    return out


def _is_stale_claim(row: dict[str, Any], now: str) -> bool:
    until = str(row.get("claim_until") or "")
    return bool(until) and until < now


def _is_open_task(row: dict[str, Any]) -> bool:
    """Whether an exported row can still participate in allocation.

    Bare ``+ACTIVE`` exports in this module are intentional because Taskwarrior's
    ``status:pending`` filter hides future-wait claims. Deleted rows retain
    their historical start/claim metadata, though, so every bare export needs
    this explicit lifecycle guard.
    """
    return str(row.get("status") or "") in ("pending", "waiting")


def _scope_filter(
    actor: str, lane_filter: list[str] | None, *, include_origin: bool = False
) -> list[str]:
    private = f"project:{config.private_project(actor)}"
    origin = f"origin_thread.is:{actor}" if include_origin else ""
    if not lane_filter:
        if origin:
            return ["(", private, "or", origin, ")"]
        return [private]
    if private in lane_filter:
        if not origin or origin in lane_filter:
            return lane_filter
        return ["(", origin, "or", *lane_filter, ")"]
    if origin:
        return ["(", private, "or", origin, "or", *lane_filter, ")"]
    return ["(", private, "or", *lane_filter, ")"]


def _route_includes_origin(route: dict[str, Any] | None) -> bool:
    if route is None:
        return True
    return str(route.get("lifetime") or "") in ("Drive", "Drain")


def effective_route_filter_args(actor: str, route: dict[str, Any] | None) -> list[str]:
    return _scope_filter(
        actor,
        lanes.filter_args(route),
        include_origin=_route_includes_origin(route),
    )


def visible_rows(actor: str, filters: list[str]) -> list[dict[str, Any]]:
    return visible_rows_with_scope(actor, filters)[0]


def visible_rows_in_scope(filters: list[str], scope: list[str]) -> list[dict[str, Any]]:
    """Export one query through an already-resolved actor route scope."""
    return tw.export([*filters, *scope])


def visible_rows_with_scope(
    actor: str, filters: list[str]
) -> tuple[list[dict[str, Any]], list[str]]:
    route = lanes.team_route_for_actor(actor)
    scope = effective_route_filter_args(actor, route)
    return visible_rows_in_scope(filters, scope), scope


def visible_ready_rows(
    actor: str, *, scope: list[str] | None = None
) -> list[dict[str, Any]]:
    filters = ["status:pending", "+READY", "-ACTIVE"]
    rows = (
        visible_rows(actor, filters)
        if scope is None
        else visible_rows_in_scope(filters, scope)
    )
    return [r for r in rows if not is_hidden(r) and not str(r.get("claim_by") or "")]


def ordered_visible_ready_rows(actor: str) -> list[dict[str, Any]]:
    """The allocator's current ready order without claiming any row."""
    ready = visible_ready_rows(actor)
    if not ready:
        return []
    active_rows = [r for r in tw.export(["+ACTIVE"]) if _is_open_task(r)]
    if any(str(row.get("claim_by") or "") == actor for row in active_rows):
        return []
    claimed_rows = tw.export([f"claim_by.is:{actor}"])
    return order(ready, actor, claimed_rows, active_rows)


def visible_active_rows(
    actor: str, *, scope: list[str] | None = None
) -> list[dict[str, Any]]:
    # Bare +ACTIVE: claims preserve wait, and status:pending filters out
    # future-wait rows, which would hide claimed deferred tasks.
    filters = ["+ACTIVE"]
    rows = (
        visible_rows(actor, filters)
        if scope is None
        else visible_rows_in_scope(filters, scope)
    )
    return [
        r
        for r in rows
        if _is_open_task(r) and not is_hidden(r) and str(r.get("claim_by") or "")
    ]


def briefing_snapshot(actor: str) -> BriefingTaskSnapshot:
    """Export one board snapshot and mark rows visible through the actor's route."""
    route = lanes.team_route_for_actor(actor)
    filter_terms = tuple(lanes.effective_filter_terms(route))
    rows = tuple(tw.export(["status.any:"]))
    visible_uuids = frozenset(
        str(row.get("uuid") or "")
        for row in rows
        if _briefing_scope_matches(
            row,
            actor=actor,
            route=route,
            filter_terms=filter_terms,
        )
    )
    return BriefingTaskSnapshot(rows=rows, visible_uuids=visible_uuids)


def _briefing_scope_matches(
    row: dict[str, Any],
    *,
    actor: str,
    route: dict[str, Any] | None,
    filter_terms: tuple[str, ...],
) -> bool:
    project = str(row.get("project") or "")
    if _project_filter_matches(project, config.private_project(actor)):
        return True
    if _route_includes_origin(route) and str(row.get("origin_thread") or "") == actor:
        return True
    return any(_briefing_filter_term_matches(row, term) for term in filter_terms)


def _briefing_filter_term_matches(row: dict[str, Any], term: str) -> bool:
    if term.startswith("project:"):
        return _project_filter_matches(
            str(row.get("project") or ""), term.split(":", 1)[1]
        )
    if term.startswith("phase:"):
        return str(row.get("phase") or "") == term.split(":", 1)[1]
    if term.startswith("+"):
        tags = row.get("tags") or []
        return term[1:] in tags if isinstance(tags, list) else False
    return False


def _project_filter_matches(project: str, expected: str) -> bool:
    return project == expected or project.startswith(f"{expected}.")


def _candidate_rows(
    actor: str,
    lane_filter: list[str] | None,
    overrides: list[str],
    *,
    include_origin: bool = False,
) -> list[dict[str, Any]]:
    base_filter = ["status:pending", "+READY", "-ACTIVE"]
    return tw.export(
        [
            *base_filter,
            *_scope_filter(actor, lane_filter, include_origin=include_origin),
        ],
        overrides=overrides,
    )


def _unclaimed_actionable(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [r for r in rows if not is_hidden(r) and not str(r.get("claim_by") or "")]


def _claim_first(
    candidates: list[dict[str, Any]],
    actor: str,
    claimed_rows: list[dict[str, Any]],
    active_rows: list[dict[str, Any]],
    *,
    guard_unclaimed: bool,
) -> dict[str, Any] | None:
    site = claimstate.current_claim_site()
    for chosen in order(candidates, actor, claimed_rows, active_rows):
        if not claimstate.do_claim(
            identity.uuid_of(chosen),
            actor,
            site=site,
            context_thread=None,
            lease_seconds=None,
            guard_unclaimed=guard_unclaimed,
        ):
            # lost the race to a concurrent agent; fall through to the next one
            continue
        fresh = identity.resolve(identity.render_handle(chosen))
        if str(fresh.get("claim_by") or "") == actor:
            return fresh
    return None


def _require_current_supervisor() -> None:
    """Refuse to hand out new work when the lane's supervisor predates the store.

    The supervisor is the long-lived process, so it is the one that goes stale:
    an editable deployment reaches new processes at once and running ones never.
    This CLI is short-lived and always reads current code, so it cannot detect
    its own staleness -- it can only compare what the supervisor recorded when
    it started against what the store carries now.

    Both sides must actually say something before this refuses. A lane with no
    agent state, a supervisor from before this field existed, and a store that
    has not been created yet each leave nothing to compare, and absence is not
    evidence of staleness. Only a recorded version strictly older than the live
    stamp names a process that outlived the constant it compiled in.

    The imports are function-local so the ordinary `spice task` path does not
    pay for the agent and serve modules on every command, matching how
    `spice.serve.team.store` reaches back into task config.
    """
    from spice.agent.lifecyclebinding import (
        SUPERVISOR_SCHEMA_VERSION_FIELD,
        read_agent_state,
    )
    from spice.serve.team.store import team_database_path

    state = read_agent_state(config.repo_root())
    recorded = state.get(SUPERVISOR_SCHEMA_VERSION_FIELD)
    database = team_database_path()
    if not isinstance(recorded, int) or not database.exists():
        return
    with sqlite_connection(database) as connection:
        stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
    if recorded >= stored:
        return
    raise SpiceError(
        "supervisor for this lane imported team authority schema version "
        f"{recorded} but the store is stamped {stored}; this process predates "
        "the deployment and should wind down instead of taking new work. "
        "Finish and close the task you already hold, then exit."
    )


def next_task() -> dict[str, Any] | None:
    actor = tw.current_actor()
    active_rows = [r for r in tw.export(["+ACTIVE"]) if _is_open_task(r)]
    own_active = [r for r in active_rows if str(r.get("claim_by") or "") == actor]
    if own_active:
        return max(own_active, key=lambda r: str(r.get("claim_at") or ""))

    # Past the held-task return, so a stale lane still finishes what it holds
    # and only stops receiving work it has not started.
    _require_current_supervisor()
    route = lanes.team_route_for_actor(actor)
    overrides = actor_overrides(actor, route)
    lane_filter = lanes.filter_args(route)
    include_origin = _route_includes_origin(route)
    scoped_active = [
        r
        for r in tw.export(
            [
                "+ACTIVE",
                *_scope_filter(actor, lane_filter, include_origin=include_origin),
            ],
            overrides=overrides,
        )
        if _is_open_task(r)
    ]
    repair_candidates = _unclaimed_actionable(scoped_active)
    if repair_candidates:
        repaired = _claim_first(
            repair_candidates, actor, [], active_rows, guard_unclaimed=False
        )
        if repaired is not None:
            return repaired
    candidates = _unclaimed_actionable(
        _candidate_rows(actor, lane_filter, overrides, include_origin=include_origin),
    )
    if candidates:
        # We intend to claim: bring the tree to the current baseline once
        # before the claim records HEAD, so new work starts from the latest
        # shared state.
        for note_text in boundaries.prepare_for_claim().notes:
            print(f"task: {note_text}")
        claimed_rows = tw.export([f"claim_by.is:{actor}"])
        claimed = _claim_first(
            candidates, actor, claimed_rows, active_rows, guard_unclaimed=True
        )
        if claimed is not None:
            return claimed
    stale_candidates = _stale_takeover_candidates(actor, scoped_active)
    if not stale_candidates:
        return None
    for note_text in boundaries.prepare_for_claim().notes:
        print(f"task: {note_text}")
    return _take_over_stale(stale_candidates, actor, active_rows)


def _stale_takeover_candidates(
    actor: str, scoped_active: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    """Peer claims whose owner stopped renewing before the deadline.

    Takeover runs only when no fresh READY work exists.
    """
    now = tw.now_iso()
    return [
        r
        for r in scoped_active
        if not is_hidden(r)
        and str(r.get("claim_by") or "") not in ("", actor)
        and _is_stale_claim(r, now)
    ]


def _take_over_stale(
    candidates: list[dict[str, Any]],
    actor: str,
    active_rows: list[dict[str, Any]],
) -> dict[str, Any] | None:
    site = claimstate.current_claim_site()
    for chosen in order(candidates, actor, [], active_rows):
        previous = str(chosen.get("claim_by") or "")
        previous_until = str(chosen.get("claim_until") or "")
        if not claimstate.take_over_stale_claim(
            identity.uuid_of(chosen),
            actor,
            expected_owner=previous,
            expected_until=previous_until,
            site=site,
            context_thread=None,
            lease_seconds=None,
        ):
            # The observed owner or lease changed after export. In particular,
            # never replace the live lease installed by another takeover.
            continue
        fresh = identity.resolve(identity.render_handle(chosen))
        if str(fresh.get("claim_by") or "") != actor:
            # lost the takeover race to a concurrent agent; try the next one
            continue
        claimstate.annotate(
            identity.uuid_of(fresh),
            f"stale claim reassigned: {previous} -> {actor}",
        )
        notice = _notify_displaced_claimant(
            chosen,
            new_owner=actor,
            expected_until=previous_until,
        )
        claimstate.annotate(
            identity.uuid_of(fresh),
            f"stale claim reassignment notice: {notice}",
        )
        return fresh
    return None


def _notify_displaced_claimant(
    stale_row: dict[str, Any],
    *,
    new_owner: str,
    expected_until: str,
) -> str:
    """Tell the displaced lane through its durable ordinary inbox."""
    target_text = str(stale_row.get("claim_worktree") or "").strip()
    if not target_text:
        return "target-unavailable claim_worktree-empty"
    target = Path(target_text)
    if not target.is_dir():
        return f"target-unavailable target={target}"
    try:
        if target.resolve() == config.repo_root().resolve():
            return f"same-worktree target={target}"
    except OSError:
        return f"target-unavailable target={target}"
    handle = identity.render_handle(stale_row)
    previous = str(stale_row.get("claim_by") or "")
    body = (
        f"[CLAIM] {handle} was reassigned from your lane ({previous}) to "
        f"{new_owner} after its recorded lease expired at {expected_until}. "
        "Stop editing that task; capture any work that must continue before "
        "attempting to land it."
    )
    try:
        path = write_inbox_item(
            target,
            default_inbox_name(),
            compose_inbox_text(body=body, priority="review", stop=False),
            dedupe_pending_text=True,
        )
    except (OSError, RuntimeError) as exc:
        return f"delivery-failed target={target} detail={exc}"
    return f"delivered key={inbox_item_key(path.name)} target={target}"
