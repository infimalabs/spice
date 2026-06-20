"""Allocator policy for `task next`: per-agent urgency, stickiness, anti-affinity.

Native urgency ranks first (computed by Taskwarrior under the actor's rc
overrides — anti-self-review plus any lane overlay). Within the top urgency
band, `task next` avoids cells a peer is actively on (spread) and prefers the
smallest move from the actor's last cell (stick).
"""

from __future__ import annotations

from typing import Any

from spice.tasks import lanes

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
