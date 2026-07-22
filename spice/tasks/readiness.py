"""Durable queue-age origins for rows that enter Taskwarrior READY.

Taskwarrior gives initial readiness an authoritative native ``entry`` stamp,
and timed blockers retain their ``wait``/``scheduled`` horizons.  Later
transitions need one Spice-owned fact because unrelated row modifications must
not reset queue age.  Mutation owners write ``ready_at`` only after the native
READY filter confirms the transition; allocator reads stay read-only.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any, Iterable

from spice.errors import SpiceError
from spice.tasks import config, identity, tw


def is_ready(uuid: str) -> bool:
    """Whether one row is currently allocatable by Taskwarrior state."""
    return bool(tw.export([uuid, "status:pending", "+READY", "-ACTIVE"]))


def ready_when_inactive(uuid: str) -> bool:
    """Whether clearing ACTIVE would expose this row as READY."""
    return bool(tw.export([uuid, "status:pending", "+READY"]))


def transition_arg(*, at: str, ready: bool) -> str:
    """One atomic UDA mutation for a known READY or non-READY transition."""
    value = at if ready else ""
    return f"{config.TASK_READY_AT_UDA}:{value}"


def stamp_if_ready(uuid: str, *, at: str) -> bool:
    """Stamp one confirmed READY transition without touching a raced claim."""
    try:
        tw.run(
            [
                uuid,
                "status:pending",
                "+READY",
                "-ACTIVE",
                "modify",
                f"{config.TASK_READY_AT_UDA}:{at}",
            ]
        )
    except SpiceError:
        # Losing readiness to a concurrent claim is the successful competing
        # outcome, not a transition-stamping failure. Any row still READY was
        # rejected for a different reason and must remain loud.
        if not is_ready(uuid):
            return False
        raise
    return True


def prepare_ready_rows(uuids: Iterable[str], *, at: str) -> None:
    """Set a pending transition stamp before its final blocker completes.

    Rows with another blocker remain invisible and will be overwritten by the
    mutation that clears that final blocker. Preparing first keeps a failed
    dependent write from following an irreversible completion.
    """
    unique = tuple(dict.fromkeys(uuids))
    if unique:
        tw.run([*unique, "modify", transition_arg(at=at, ready=True)])


def reconcile_transition(uuid: str, *, was_ready: bool, at: str) -> None:
    """Refresh or clear the stamp when one mutation changes READY state."""
    now_ready = is_ready(uuid)
    if was_ready and not now_ready:
        tw.run([uuid, "modify", transition_arg(at=at, ready=False)])
    elif now_ready and not was_ready:
        stamp_if_ready(uuid, at=at)


def dependents_becoming_ready(uuid: str, *, at: str) -> list[str]:
    """Rows for which completing ``uuid`` clears the final READY blocker."""
    at_epoch = _datetime_epoch(at, field="ready transition")
    rows = tw.export(["status.any:"])
    by_uuid = {identity.uuid_of(row): row for row in rows}
    result: list[str] = []
    for row in rows:
        if str(row.get("status") or "") not in ("pending", "waiting"):
            continue
        dependencies = _dependency_uuids(row)
        if uuid not in dependencies:
            continue
        remaining = [dependency for dependency in dependencies if dependency != uuid]
        if _timed_blockers_clear(row, at_epoch=at_epoch) and all(
            str(by_uuid.get(dependency, {}).get("status") or "") == "completed"
            for dependency in remaining
        ):
            result.append(identity.uuid_of(row))
    return result


def ready_after_clearing_wait(row: dict[str, Any], *, at: str) -> bool:
    """Whether an atomic ``wait:`` mutation makes this unclaimed row READY."""
    at_epoch = _datetime_epoch(at, field="ready transition")
    if not _timed_blockers_clear(row, at_epoch=at_epoch, ignore_wait=True):
        return False
    for dependency in _dependency_uuids(row):
        rows = tw.export([dependency])
        if len(rows) != 1 or str(rows[0].get("status") or "") != "completed":
            return False
    return True


def _dependency_uuids(row: dict[str, Any]) -> list[str]:
    raw = row.get("depends") or []
    dependencies = [raw] if isinstance(raw, str) else list(raw)
    return [str(dependency) for dependency in dependencies]


def _timed_blockers_clear(
    row: dict[str, Any], *, at_epoch: float, ignore_wait: bool = False
) -> bool:
    fields = ("scheduled",) if ignore_wait else ("wait", "scheduled")
    return all(
        _datetime_epoch(value, field=field) <= at_epoch
        for field in fields
        if (value := str(row.get(field) or "").strip())
    )


def queue_ready_epoch(row: dict[str, Any]) -> float:
    """Resolve the one durable origin used for starvation age.

    ``ready_at`` is authoritative for a later transition and malformed values
    fail loudly. A row without it is on its first READY interval, so native
    creation and timed-blocker stamps determine the origin.
    """
    explicit = str(row.get(config.TASK_READY_AT_UDA) or "").strip()
    if explicit:
        return _datetime_epoch(explicit, field=config.TASK_READY_AT_UDA)

    origins = [
        _datetime_epoch(value, field=field)
        for field in ("entry", "wait", "scheduled")
        if (value := str(row.get(field) or "").strip())
    ]
    if origins:
        return max(origins)

    raise SpiceError(
        "READY task is missing ready_at, entry, wait, and scheduled queue-age origins"
    )


def _datetime_epoch(value: str, *, field: str) -> float:
    try:
        if value.endswith("Z") and len(value) == len("20260722T201234Z"):
            parsed = datetime.strptime(value, tw.TW_DATETIME_FORMAT).replace(tzinfo=UTC)
        else:
            parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
            if parsed.tzinfo is None:
                raise ValueError("timezone missing")
        return parsed.timestamp()
    except (OverflowError, ValueError) as exc:
        raise SpiceError(f"task has malformed {field} timestamp: {value!r}") from exc
