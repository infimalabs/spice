"""Identity: the ``incepted`` timestamp (sole stored id) and the rendered handle.

A handle is ``KEY-INCEPTED``. ``incepted`` is a compact microsecond UTC stamp
(``YYYYMMDDThhmmssffffffZ``) — the same grammar as ACK/inbox keys — and is the
only stored identity. ``KEY`` is derived from the current project's rightmost
segment and is never stored, so re-homing changes the rendered handle for
free. Resolution matches on ``incepted``.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime, timedelta
from typing import Any

from spice.errors import SpiceError
from spice.tasks import tw

INCEPTED_RE = re.compile(r"^\d{8}T\d{12}Z$")
ZULU_FREE_INCEPTED_RE = re.compile(r"^\d{8}T\d{12}$")
_WORD_RE = re.compile(r"[A-Za-z0-9]+")
_KEY_MAX = 7
_KEY_ACRONYM_MIN_WORDS = 3


def mint_incepted(existing: set[str] | None = None) -> str:
    """Fresh ``incepted`` stamp, advanced 1µs past any collision."""
    if existing is None:
        existing = {str(r.get("incepted") or "") for r in tw.export()}
    when = datetime.now(UTC)
    while True:
        stamp = when.strftime("%Y%m%dT%H%M%S%fZ")
        if stamp not in existing:
            return stamp
        when += timedelta(microseconds=1)


def key_for(project: str | None, title: str) -> str:
    if project:
        segment = project.split(".")[-1]
        key = re.sub(r"[^0-9A-Z_]", "", segment.upper())
        if key:
            return key[:_KEY_MAX]
    words = _WORD_RE.findall(title)
    if len(words) >= _KEY_ACRONYM_MIN_WORDS:
        return "".join(w[0] for w in words[:_KEY_MAX]).upper() or "TASK"
    compact = "".join(words).upper()
    return (compact or "TASK")[:_KEY_MAX]


def render_handle(row: dict[str, Any]) -> str:
    incepted = str(row.get("incepted") or "").strip()
    if not incepted:
        return str(row.get("uuid") or "?")
    key = key_for(
        str(row.get("project") or "") or None, str(row.get("description") or "")
    )
    return f"{key}-{incepted}"


def canonicalize_zulu_free_handle(handle: str) -> tuple[str, bool]:
    value = handle.strip()
    if ZULU_FREE_INCEPTED_RE.match(value):
        return f"{value}Z", True
    if "-" in value:
        key, tail = value.split("-", 1)
        if ZULU_FREE_INCEPTED_RE.match(tail):
            return f"{key}-{tail}Z", True
    return value, False


def incepted_of_handle(handle: str) -> str:
    """Extract the ``incepted`` portion from a handle (or a bare stamp)."""
    value, _added_z = canonicalize_zulu_free_handle(handle)
    if INCEPTED_RE.match(value):
        return value
    if "-" in value:
        tail = value.split("-", 1)[1]
        if INCEPTED_RE.match(tail):
            return tail
    raise SpiceError(f"not a valid task handle: {handle!r}")


def resolve(handle: str) -> dict[str, Any]:
    """Resolve a handle (or bare incepted, or uuid) to exactly one row."""
    value = handle.strip()
    rows: list[dict[str, Any]]
    if INCEPTED_RE.match(value) or "-" in value:
        incepted = incepted_of_handle(value)
        rows = [r for r in tw.export() if str(r.get("incepted") or "") == incepted]
    else:
        rows = tw.export([value])
    if not rows:
        raise SpiceError(f"unknown task: {handle}")
    if len(rows) > 1:
        raise SpiceError(f"ambiguous task: {handle}")
    return rows[0]


def uuid_of(row: dict[str, Any]) -> str:
    uuid = str(row.get("uuid") or "").strip()
    if not uuid:
        raise SpiceError("task row has no uuid")
    return uuid
