"""Identity: the ``incepted`` stamp (sole stored id) and the rendered handle.

A handle is ``KEY-INCEPTED``. ``incepted`` is a fixed-width 8-character base52
encoding of the inception time in epoch milliseconds, minted via the
order-preserving codec below — short, yet sortable as a plain string. It is the
only stored identity. ``KEY`` is derived from the current project's rightmost
segment and is never stored, so re-homing changes the rendered handle for
free. Resolution matches on ``incepted``. Human-readable inception time stays
available from Taskwarrior's ``entry`` field.

The base52 alphabet drops both-case vowels so a stamp can never spell a word.
The remaining digits-then-consonants run stays ASCII-monotonic, so a
fixed-width, zero-padded stamp sorts lexicographically in the same order as the
millisecond value it encodes.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime
from typing import Any

from spice.errors import SpiceError
from spice.tasks import tw

# Vowels (both cases) are excluded so a stamp can never spell a word; the
# remaining digits-then-consonants sequence stays ASCII-monotonic, so a
# fixed-width zero-padded stamp still sorts chronologically. Base52 is smaller
# than base62, so the stamp needs one more character to hold epoch ms.
ALPHABET = "0123456789BCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz"
BASE = len(ALPHABET)
ZERO = ALPHABET[0]
STAMP_WIDTH = 8
MILLIS_PER_SECOND = 1000

INCEPTED_RE = re.compile(rf"^[{ALPHABET}]{{{STAMP_WIDTH}}}$")
_VALUES = {char: index for index, char in enumerate(ALPHABET)}
_WORD_RE = re.compile(r"[A-Za-z0-9]+")
_KEY_MAX = 7
_KEY_ACRONYM_MIN_WORDS = 3


def encode(value: int) -> str:
    """Encode a non-negative integer as base62 (no padding)."""
    if value < 0:
        raise ValueError(f"base62 cannot encode a negative value: {value}")
    if value == 0:
        return ZERO
    digits: list[str] = []
    while value:
        value, remainder = divmod(value, BASE)
        digits.append(ALPHABET[remainder])
    return "".join(reversed(digits))


def decode(text: str) -> int:
    """Decode a base62 string back to its integer value."""
    if not text:
        raise ValueError("base62 cannot decode an empty string")
    value = 0
    for char in text:
        digit = _VALUES.get(char)
        if digit is None:
            raise ValueError(f"invalid base62 character: {char!r}")
        value = value * BASE + digit
    return value


def encode_width(value: int, width: int = STAMP_WIDTH) -> str:
    """Encode ``value`` as a fixed-width, zero-padded base62 string.

    Fixed width is what keeps the encoding order-preserving under a string
    sort; an oversized value is an error rather than a silent sort break.
    """
    encoded = encode(value)
    if len(encoded) > width:
        raise ValueError(f"value {value} does not fit in {width} base62 chars")
    return encoded.rjust(width, ZERO)


def epoch_millis(when: datetime | None = None) -> int:
    """Whole milliseconds since the Unix epoch for ``when`` (default: now)."""
    moment = when if when is not None else datetime.now(UTC)
    return int(moment.timestamp() * MILLIS_PER_SECOND)


def incepted_datetime(incepted: str) -> datetime:
    """The aware UTC instant encoded by an ``incepted`` stamp."""
    return datetime.fromtimestamp(decode(incepted) / MILLIS_PER_SECOND, UTC)


def mint_incepted(existing: set[str] | None = None) -> str:
    """Fresh ``incepted`` stamp, advanced 1ms past any collision."""
    if existing is None:
        existing = {str(r.get("incepted") or "") for r in tw.export()}
    millis = epoch_millis()
    while True:
        stamp = encode_width(millis)
        if stamp not in existing:
            return stamp
        millis += 1


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


def incepted_of_handle(handle: str) -> str:
    """Extract the ``incepted`` portion from a handle (or a bare stamp)."""
    value = handle.strip()
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
