"""Allocator policy for `task next`: per-agent urgency, stickiness, anti-affinity.

Native urgency ranks first (computed by Taskwarrior under the actor's rc
overrides — anti-self-review plus any lane overlay). Within the top urgency
band, `task next` avoids cells a peer is actively on (spread) and prefers the
smallest move from the actor's last cell (stick).

A review the actor authored is not merely outranked by that coefficient — it is
dropped from every candidate set, so a quiet board cannot hand an author their
own review.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.mail.inbox import (
    compose_inbox_text,
    default_inbox_name,
    inbox_item_key,
    write_inbox_item,
)
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


def visible_rows_with_scope(
    actor: str, filters: list[str]
) -> tuple[list[dict[str, Any]], list[str]]:
    route = lanes.team_route_for_actor(actor)
    scope = effective_route_filter_args(actor, route)
    return tw.export([*filters, *scope]), scope


def visible_ready_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending", "+READY", "-ACTIVE"])
    return _allocatable(
        [r for r in rows if not is_hidden(r) and not str(r.get("claim_by") or "")],
        actor,
    )


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


def visible_active_rows(actor: str) -> list[dict[str, Any]]:
    # Bare +ACTIVE: claims preserve wait, and status:pending filters out
    # future-wait rows, which would hide claimed deferred tasks.
    rows = visible_rows(actor, ["+ACTIVE"])
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


def _allocatable(rows: list[dict[str, Any]], actor: str) -> list[dict[str, Any]]:
    """Drop rows this actor may not be handed, whatever else recommends them.

    A review authored by this actor is refused outright rather than merely
    deprioritized by `ANTI_SELF_REVIEW`: a coefficient only loses while some
    other work outranks it, so on a quiet board the author gets their own
    review back. Refusing here leaves the row unclaimed and available to a
    different reviewer, and the actor falls through to other work or to an
    explicit no-available-tasks answer.
    """
    return [r for r in rows if not claimstate.is_same_author_review(r, actor)]


def _unclaimed_actionable(
    rows: list[dict[str, Any]], actor: str
) -> list[dict[str, Any]]:
    return _allocatable(
        [r for r in rows if not is_hidden(r) and not str(r.get("claim_by") or "")],
        actor,
    )


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


def next_task() -> dict[str, Any] | None:
    actor = tw.current_actor()
    active_rows = [r for r in tw.export(["+ACTIVE"]) if _is_open_task(r)]
    own_active = [r for r in active_rows if str(r.get("claim_by") or "") == actor]
    if own_active:
        return max(own_active, key=lambda r: str(r.get("claim_at") or ""))

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
    repair_candidates = _unclaimed_actionable(scoped_active, actor)
    if repair_candidates:
        repaired = _claim_first(
            repair_candidates, actor, [], active_rows, guard_unclaimed=False
        )
        if repaired is not None:
            return repaired
    candidates = _unclaimed_actionable(
        _candidate_rows(actor, lane_filter, overrides, include_origin=include_origin),
        actor,
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

    Takeover runs only when no fresh READY work exists. Reviews this actor
    authored stay off-limits even when stale.
    """
    now = tw.now_iso()
    return _allocatable(
        [
            r
            for r in scoped_active
            if not is_hidden(r)
            and str(r.get("claim_by") or "") not in ("", actor)
            and _is_stale_claim(r, now)
        ],
        actor,
    )


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
