"""Suspect-wording review marker gates and resolution."""

from __future__ import annotations

from typing import Any

from spice.errors import SpiceError
from spice.tasks import config, identity, tw


def require_integrate_allowed(label: str, meta: dict[str, str] | None) -> None:
    if not meta or meta.get("phase") != "plan":
        return
    rows = tw.export([meta["uuid"]]) if meta.get("uuid") else []
    row = rows[0] if rows else identity.resolve(label)
    require_plan_phase_marker_cleared(row)


def require_plan_phase_marker_cleared(row: dict[str, Any]) -> None:
    if str(row.get("phase") or "") != "plan":
        return
    if not str(row.get(config.TASK_WORDING_REVIEW_UDA) or "").strip():
        return
    handle = identity.render_handle(row)
    raise SpiceError(
        f"task done blocked: {handle} still requires suspect-wording "
        "self-correction. Enrich the plan and acceptance criteria, then run "
        f'`spice task resolve-wording {handle} --reason "..."` before '
        "advancing out of plan."
    )


def resolve_wording_review(handle: str | None, *, reason: str) -> str:
    reason = reason.strip()
    if not reason:
        raise SpiceError("task resolve-wording requires --reason")
    from spice.tasks import claimstate

    row = claimstate.resolve_claim_target(handle, action="resolve wording review")
    handle_text = identity.render_handle(row)
    claimstate._require_pending(row, "resolve wording review")
    actor = tw.current_actor()
    _require_resolution_authority(row, actor)
    if not str(row.get(config.TASK_WORDING_REVIEW_UDA) or "").strip():
        raise SpiceError(f"{handle_text} has no suspect-wording review marker")
    uuid = identity.uuid_of(row)
    tw.run([uuid, "modify", f"{config.TASK_WORDING_REVIEW_UDA}:"])
    claimstate.annotate(uuid, f"wording review resolved: {reason}")
    return f"resolved wording review for {handle_text}"


def _require_resolution_authority(row: dict[str, Any], actor: str) -> None:
    """Row owner, or holder of a plan parent directly depending on the row.

    Plan phase owns child board state, and the parent's claim blocks claiming
    the child, so the parent holder must be able to resolve an unclaimed
    child's marker in place without touching either claim.
    """
    from spice.tasks import claimstate

    unclaimed = not str(row.get("claim_by") or "") and not bool(row.get("start"))
    if unclaimed and _connected_plan_parent(row, actor) is not None:
        return
    claimstate._require_owner(row, actor, "resolve wording review")


def _connected_plan_parent(row: dict[str, Any], actor: str) -> dict[str, Any] | None:
    from spice.tasks import claimstate

    child_uuid = identity.uuid_of(row)
    for parent in claimstate._active_claims_for(actor):
        if str(parent.get("phase") or "") != "plan":
            continue
        if child_uuid in _dependency_uuids(parent):
            return parent
    return None


def _dependency_uuids(row: dict[str, Any]) -> list[str]:
    raw = row.get("depends") or []
    if isinstance(raw, str):
        return [raw] if raw else []
    return [str(item) for item in raw if str(item)]
