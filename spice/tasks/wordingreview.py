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
    from spice.tasks import ops

    row = ops.resolve_claim_target(handle, action="resolve wording review")
    handle_text = identity.render_handle(row)
    ops._require_pending(row, "resolve wording review")
    actor = tw.current_actor()
    ops._require_owner(row, actor, "resolve wording review")
    if not str(row.get(config.TASK_WORDING_REVIEW_UDA) or "").strip():
        raise SpiceError(f"{handle_text} has no suspect-wording review marker")
    uuid = identity.uuid_of(row)
    tw.run([uuid, "modify", f"{config.TASK_WORDING_REVIEW_UDA}:"])
    ops.annotate(uuid, f"wording review resolved: {reason}")
    return f"resolved wording review for {handle_text}"
