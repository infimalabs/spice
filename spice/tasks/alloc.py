"""Allocator policy for `task next`: per-agent urgency, stickiness, anti-affinity.

Native urgency ranks first (computed by Taskwarrior under the actor's rc
overrides — anti-self-review plus any lane overlay). Within the top urgency
band, `task next` avoids cells a peer is actively on (spread) and prefers the
smallest move from the actor's last cell (stick).
"""

from __future__ import annotations

from typing import Any

from spice.tasks import config, gitsync, identity, lanes, tw

ANTI_SELF_REVIEW = -100.0  # make self-authored reviews lose to ordinary work
BAND_WIDTH = 5.0  # urgency window treated as "top band" for tie-breaks


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
    return "oops" in (row.get("tags") or [])


def oops_rows() -> list[dict[str, Any]]:
    """Deferred oops items carry a far-future wait, so they are `waiting`."""
    return [
        r
        for r in tw.export(["+oops"])
        if str(r.get("status")) in ("pending", "waiting")
    ]


def stale_rows() -> list[dict[str, Any]]:
    """Active claims whose deadline has elapsed (claim_until < now). ISO-8601
    timestamps share a format here, so a lexicographic compare is
    chronological."""
    now = tw.now_iso()
    out: list[dict[str, Any]] = []
    for r in tw.export(["+ACTIVE"]):
        until = str(r.get("claim_until") or "")
        if until and until < now:
            out.append(r)
    return out


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
    route = lanes.team_route_for_actor(actor)
    return tw.export(
        [
            *filters,
            *effective_route_filter_args(actor, route),
        ]
    )


def visible_ready_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending", "+READY", "-ACTIVE"])
    return [r for r in rows if not is_oops(r) and not str(r.get("claim_by") or "")]


def visible_active_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending", "+ACTIVE"])
    return [r for r in rows if not is_oops(r) and str(r.get("claim_by") or "")]


def visible_pending_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending"])
    return [r for r in rows if not is_oops(r)]


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
    return [r for r in rows if not is_oops(r) and not str(r.get("claim_by") or "")]


def _claim_first(
    candidates: list[dict[str, Any]],
    actor: str,
    claimed_rows: list[dict[str, Any]],
    active_rows: list[dict[str, Any]],
    *,
    guard_unclaimed: bool,
) -> dict[str, Any] | None:
    from spice.tasks import ops

    for chosen in order(candidates, actor, claimed_rows, active_rows):
        if not ops.do_claim(
            identity.uuid_of(chosen), actor, guard_unclaimed=guard_unclaimed
        ):
            # lost the race to a concurrent agent; fall through to the next one
            continue
        fresh = identity.resolve(identity.render_handle(chosen))
        if str(fresh.get("claim_by") or "") == actor:
            return fresh
    return None


def next_task() -> dict[str, Any] | None:
    actor = tw.current_actor()
    active_rows = tw.export(["status:pending", "+ACTIVE"])
    own_active = [r for r in active_rows if str(r.get("claim_by") or "") == actor]
    if own_active:
        return max(own_active, key=lambda r: str(r.get("claim_at") or ""))

    route = lanes.team_route_for_actor(actor)
    overrides = actor_overrides(actor, route)
    lane_filter = lanes.filter_args(route)
    include_origin = _route_includes_origin(route)
    repair_candidates = _unclaimed_actionable(
        tw.export(
            [
                "status:pending",
                "+ACTIVE",
                *_scope_filter(actor, lane_filter, include_origin=include_origin),
            ],
            overrides=overrides,
        )
    )
    if repair_candidates:
        repaired = _claim_first(
            repair_candidates, actor, [], active_rows, guard_unclaimed=False
        )
        if repaired is not None:
            return repaired
    candidates = _unclaimed_actionable(
        _candidate_rows(actor, lane_filter, overrides, include_origin=include_origin)
    )
    if not candidates:
        return None
    # We intend to claim: bring the tree to the current baseline once before
    # the claim records HEAD, so new work starts from the latest shared state.
    for note_text in gitsync.prepare_for_claim().notes:
        print(f"task: {note_text}")
    claimed_rows = tw.export([f"claim_by.is:{actor}"])
    return _claim_first(
        candidates, actor, claimed_rows, active_rows, guard_unclaimed=True
    )
