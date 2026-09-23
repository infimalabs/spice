"""Identity: the ``incepted`` stamp (sole stored id) and the rendered handle.

A handle is ``KEY-INCEPTED``. New ``incepted`` stamps encode Unix microseconds
in nine base52 characters; retained eight-character stamps encode milliseconds.
The stamp is the only stored identity, and existing stamps keep their spelling.
``KEY`` is derived from the project's rightmost segment and is never stored, so
re-homing changes the rendered handle for free. Resolution matches on
``incepted``. Human-readable inception time stays
available from Taskwarrior's ``entry`` field.

The base52 alphabet drops both-case vowels so a stamp can never spell a word.
The remaining digits-then-consonants run stays ASCII-monotonic, so a
fixed-width, zero-padded stamp sorts lexicographically in the same order as the
value it encodes within one width. Across widths, compare normalized microseconds.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from datetime import UTC, datetime, timedelta
from typing import Any

from spice.errors import SpiceError
from spice.tasks import tw

# Vowels (both cases) are excluded so a stamp can never spell a word; the
# remaining digits-then-consonants sequence stays ASCII-monotonic, so a
# fixed-width zero-padded stamp still sorts chronologically within its generation.
ALPHABET = "0123456789BCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz"
BASE = len(ALPHABET)
ZERO = ALPHABET[0]
STAMP_WIDTH = 9
LEGACY_STAMP_WIDTH = 8
STAMP_WIDTHS = (STAMP_WIDTH, LEGACY_STAMP_WIDTH)
MICROS_PER_MILLISECOND = 1000
_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)
_MICROSECOND = timedelta(microseconds=1)

STAMP_PATTERN = rf"[{ALPHABET}]{{{LEGACY_STAMP_WIDTH},{STAMP_WIDTH}}}"
INCEPTED_RE = re.compile(rf"\A{STAMP_PATTERN}\Z")
_VALUES = {char: index for index, char in enumerate(ALPHABET)}
_WORD_RE = re.compile(r"[A-Za-z0-9]+")
_KEY_MAX = 7
_KEY_ACRONYM_MIN_WORDS = 3


def encode(value: int) -> str:
    """Encode a non-negative integer as base52 (no padding)."""
    if value < 0:
        raise ValueError(f"base52 cannot encode a negative value: {value}")
    if value == 0:
        return ZERO
    digits: list[str] = []
    while value:
        value, remainder = divmod(value, BASE)
        digits.append(ALPHABET[remainder])
    return "".join(reversed(digits))


def decode(text: str) -> int:
    """Decode a base52 string back to its integer value."""
    if not text:
        raise ValueError("base52 cannot decode an empty string")
    value = 0
    for char in text:
        digit = _VALUES.get(char)
        if digit is None:
            raise ValueError(f"invalid base52 character: {char!r}")
        value = value * BASE + digit
    return value


def encode_width(value: int, width: int = STAMP_WIDTH) -> str:
    """Encode ``value`` as a fixed-width, zero-padded base52 string.

    Fixed width is what keeps the encoding order-preserving under a string
    sort; an oversized value is an error rather than a silent sort break.
    """
    encoded = encode(value)
    if len(encoded) > width:
        raise ValueError(f"value {value} does not fit in {width} base52 chars")
    return encoded.rjust(width, ZERO)


def epoch_micros(when: datetime | None = None) -> int:
    """Exact whole microseconds since the Unix epoch (default: now)."""
    moment = when if when is not None else datetime.now(UTC)
    return (moment.astimezone(UTC) - _EPOCH) // _MICROSECOND


def incepted_micros(incepted: str) -> int:
    """Normalize a current or retained stamp without changing its identity."""
    if not INCEPTED_RE.fullmatch(incepted):
        raise ValueError(f"invalid inception stamp: {incepted!r}")
    value = decode(incepted)
    if len(incepted) == LEGACY_STAMP_WIDTH:
        return value * MICROS_PER_MILLISECOND
    return value


def incepted_datetime(incepted: str) -> datetime:
    """The aware UTC instant encoded by an ``incepted`` stamp."""
    return _EPOCH + timedelta(microseconds=incepted_micros(incepted))


def mint_incepted(existing: set[str] | None = None) -> str:
    """Fresh nine-character stamp, advanced one microsecond past collisions."""
    if existing is None:
        existing = {str(r.get("incepted") or "") for r in tw.export()}
    micros = epoch_micros()
    while True:
        stamp = encode_width(micros)
        if stamp not in existing:
            return stamp
        micros += 1


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


def render_handle(row: Mapping[str, Any]) -> str:
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
